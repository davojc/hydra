//! A local stand-in for GitHub's release API, for tests (feature `fake`).
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sha2::{Digest, Sha256};

pub struct FakeGitHub {
    base: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl FakeGitHub {
    /// Serves a latest release `tag` with `assets` (name, bytes). Each asset gets a correct
    /// `<name>.sha256` unless `assets` names one itself.
    pub fn serve(tag: &str, assets: &[(&str, &[u8])]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut routes: HashMap<String, Vec<u8>> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        for (name, bytes) in assets {
            routes.insert(format!("/dl/{name}"), bytes.to_vec());
            names.push(name.to_string());
        }
        for (name, bytes) in assets {
            let sha = format!("{name}.sha256");
            if !name.ends_with(".sha256") && !routes.contains_key(&format!("/dl/{sha}")) {
                routes.insert(
                    format!("/dl/{sha}"),
                    format!("{}  {name}\n", sha256_hex(bytes)).into_bytes(),
                );
                names.push(sha);
            }
        }
        let list: Vec<String> = names
            .iter()
            .map(|n| format!(r#"{{"name":"{n}","browser_download_url":"{base}/dl/{n}"}}"#))
            .collect();
        let json = format!(r#"{{"tag_name":"{tag}","assets":[{}]}}"#, list.join(","));
        routes.insert(
            "/repos/davojc/hydra/releases/latest".into(),
            json.into_bytes(),
        );
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer(stream, &routes);
            }
        });
        Self { base }
    }

    /// The latest-release URL to pass to `fetch_latest`.
    pub fn latest_url(&self) -> String {
        format!("{}/repos/davojc/hydra/releases/latest", self.base)
    }
}

fn answer(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match routes.get(path) {
        Some(b) => ("200 OK", b.clone()),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}
