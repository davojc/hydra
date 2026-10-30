//! Device endpoints, the channel between your own machines, over real iroh endpoints with relays
//! off, on loopback.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use hydra_swarms::device::{DeviceHandler, DeviceNode};
use hydra_swarms::wire::{MineReply, MineRequest};

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("random bytes");
    b
}

/// Answers `Ok` to everything and remembers who asked what.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, MineRequest)>>);

impl DeviceHandler for Recorder {
    fn answer(&self, from: String, req: MineRequest) -> BoxFuture<'static, MineReply> {
        self.0.lock().unwrap().push((from, req));
        async { MineReply::Ok }.boxed()
    }
}

/// Never answers.
struct Silent;

impl DeviceHandler for Silent {
    fn answer(&self, _: String, _: MineRequest) -> BoxFuture<'static, MineReply> {
        futures::future::pending().boxed()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_reaches_the_handler_with_the_callers_id() {
    let recorder = Arc::new(Recorder::default());
    let host = DeviceNode::start(random32(), false, recorder.clone())
        .await
        .unwrap();
    let caller = DeviceNode::start(random32(), false, Arc::new(Recorder::default()))
        .await
        .unwrap();
    caller.remember_addr(host.addr_for_tests());
    let req = MineRequest::Link {
        token: "t".into(),
        name: "laptop".into(),
    };
    let reply = caller.ask(&host.id(), &req).await.unwrap();
    assert_eq!(reply, MineReply::Ok);
    assert_eq!(*recorder.0.lock().unwrap(), vec![(caller.id(), req)]);
    caller.shutdown().await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_handler_times_out_the_caller() {
    let host = DeviceNode::start(random32(), false, Arc::new(Silent))
        .await
        .unwrap();
    let caller = DeviceNode::start(random32(), false, Arc::new(Silent))
        .await
        .unwrap();
    caller.remember_addr(host.addr_for_tests());
    let req = MineRequest::Link {
        token: "t".into(),
        name: "laptop".into(),
    };
    let started = std::time::Instant::now();
    let asked = caller
        .ask_within(&host.id(), &req, Duration::from_secs(2))
        .await;
    assert!(asked.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    caller.shutdown().await;
    host.shutdown().await;
}
