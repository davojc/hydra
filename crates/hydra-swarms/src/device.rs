//! This machine's device endpoint: one stable iroh identity, used only between your own
//! machines (`hydra/mine/1`). It hands every request to a handler with the caller's verified id;
//! the handler decides who may ask what.
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::future::BoxFuture;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};

use crate::node::endpoint_builder;
use crate::wire::{MINE_ALPN, MineReply, MineRequest, read_msg, write_msg};

/// How long either side waits on the other for one request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Answers your machines' requests. `from` is the caller's device id.
pub trait DeviceHandler: Send + Sync + 'static {
    fn answer(&self, from: String, req: MineRequest) -> BoxFuture<'static, MineReply>;
}

pub struct DeviceNode {
    endpoint: Endpoint,
    router: Router,
    addrs: MemoryLookup,
}

impl DeviceNode {
    /// Binds the endpoint `device_key` makes and starts answering with `handler`.
    pub async fn start(
        device_key: [u8; 32],
        relays: bool,
        handler: Arc<dyn DeviceHandler>,
    ) -> anyhow::Result<DeviceNode> {
        let addrs = MemoryLookup::new();
        let endpoint = endpoint_builder(&device_key, relays)
            .address_lookup(addrs.clone())
            .bind()
            .await?;
        let router = Router::builder(endpoint.clone())
            .accept(MINE_ALPN, Mine(handler))
            .spawn();
        Ok(DeviceNode {
            endpoint,
            router,
            addrs,
        })
    }

    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// The relay this endpoint is reachable through, if any yet, for a link code.
    pub fn relay(&self) -> Option<String> {
        self.endpoint
            .addr()
            .relay_urls()
            .next()
            .map(|u| u.to_string())
    }

    pub fn remember_addr(&self, addr: EndpointAddr) {
        self.addrs.add_endpoint_info(addr);
    }

    /// This endpoint's id with its bound sockets on loopback, for dialling it in tests.
    pub fn addr_for_tests(&self) -> EndpointAddr {
        self.endpoint
            .bound_sockets()
            .into_iter()
            .map(|s| {
                let ip = if s.is_ipv4() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    Ipv6Addr::LOCALHOST.into()
                };
                SocketAddr::new(ip, s.port())
            })
            .fold(EndpointAddr::new(self.endpoint.id()), |a, s| {
                a.with_ip_addr(s)
            })
    }

    /// Asks the machine whose device id is `to`.
    pub async fn ask(&self, to: &str, req: &MineRequest) -> anyhow::Result<MineReply> {
        self.ask_within(to, req, REQUEST_TIMEOUT).await
    }

    /// `ask`, giving up after `limit`.
    pub async fn ask_within(
        &self,
        to: &str,
        req: &MineRequest,
        limit: Duration,
    ) -> anyhow::Result<MineReply> {
        let id: EndpointId = to.parse().context("not a device id")?;
        let asked = async {
            let conn = self
                .endpoint
                .connect(EndpointAddr::new(id), MINE_ALPN)
                .await?;
            let (mut send, mut recv) = conn.open_bi().await?;
            write_msg(&mut send, req).await?;
            send.finish()?;
            let reply = read_msg(&mut recv)
                .await?
                .context("your other machine hung up without replying")?;
            conn.close(0u32.into(), b"done");
            anyhow::Ok(reply)
        };
        tokio::time::timeout(limit, asked)
            .await
            .context("your other machine didn't answer in time")?
    }

    pub async fn shutdown(&self) {
        let _ = self.router.shutdown().await;
        self.endpoint.close().await;
    }
}

#[derive(Clone)]
struct Mine(Arc<dyn DeviceHandler>);

impl std::fmt::Debug for Mine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mine")
    }
}

impl ProtocolHandler for Mine {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let from = conn.remote_id().to_string();
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            let read = tokio::time::timeout(REQUEST_TIMEOUT, read_msg::<_, MineRequest>(&mut recv));
            let Ok(Ok(Some(req))) = read.await else {
                break;
            };
            let reply = self.0.answer(from.clone(), req).await;
            if write_msg(&mut send, &reply).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}
