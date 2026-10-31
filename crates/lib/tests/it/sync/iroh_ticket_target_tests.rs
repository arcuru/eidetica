//! Dial observations, not an outbound-target policy or a successful sync test.
//! Run only in the isolated namespace described in internal/testing.md.

use std::{net::ToSocketAddrs, time::Duration};

use eidetica::sync::{
    Address,
    protocol::SyncRequest,
    transports::{SyncTransport, iroh::IrohTransport},
};
use iroh::{EndpointAddr, SecretKey, endpoint::PortmapperConfig};
use iroh_tickets::{Ticket, endpoint::EndpointTicket};
use tokio::{
    net::{TcpListener, UdpSocket},
    time::timeout,
};

const WAIT: Duration = Duration::from_secs(10);
const RELAY_HOST: &str = "ticket-relay.test";

fn isolated_transport() -> IrohTransport {
    let routes = std::fs::read_to_string("/proc/net/route").unwrap();
    assert!(
        routes
            .lines()
            .skip(1)
            .all(|line| line.split_whitespace().next() == Some("lo")),
        "Run in a loopback-only network namespace, not on the host network"
    );
    std::net::UdpSocket::bind("10.77.0.1:0")
        .expect("namespace must have the private fixture address");
    IrohTransport::builder()
        // Keep the inherited relay transport; disable unrelated discovery traffic.
        .n0_dns(false)
        .mdns(false)
        .portmapper_config(PortmapperConfig::Disabled)
        .build()
        .unwrap()
}

#[tokio::test]
#[ignore = "Requires isolated network namespace; see internal/testing.md"]
async fn test_iroh_ticket_target_direct_literals() {
    for ip in ["127.0.0.1", "10.77.0.1"] {
        let transport = isolated_transport();
        let listener = UdpSocket::bind(format!("{ip}:0")).await.unwrap();
        let target = listener.local_addr().unwrap();
        let request = SyncRequest::SendEntries(vec![]);
        let mut packet = [0u8; 2048];

        // Decode failure is a control, not evidence of dial-side filtering.
        let invalid = transport
            .send_request(&Address::iroh("invalid-ticket"), &request)
            .await;
        assert!(invalid.is_err());
        assert!(
            timeout(Duration::from_millis(100), listener.recv_from(&mut packet))
                .await
                .is_err()
        );

        let endpoint_addr =
            EndpointAddr::new(SecretKey::from_bytes(&[42; 32]).public()).with_ip_addr(target);
        let address = Address::iroh(EndpointTicket::new(endpoint_addr).encode_string());
        let observation = async {
            let (length, source) = listener.recv_from(&mut packet).await.unwrap();
            assert!(length > 0);
            eprintln!(
                "ticket target {target}: received {length} UDP bytes from {source}; dial attempted"
            );
        };
        timeout(WAIT, async {
            tokio::select! {
                () = observation => {},
                result = transport.send_request(&address, &request) => {
                    panic!("request ended before UDP observation: {result:?}");
                }
            }
        })
        .await
        .expect("ticket literal must produce an observable UDP dial attempt");
    }
}

#[tokio::test]
#[ignore = "Requires isolated network namespace and private hosts mapping; see internal/testing.md"]
async fn test_iroh_ticket_target_private_relay_hostname() {
    let transport = isolated_transport();
    let resolved: Vec<_> = (RELAY_HOST, 443).to_socket_addrs().unwrap().collect();
    assert!(!resolved.is_empty());
    assert!(
        resolved
            .iter()
            .all(|addr| addr.ip().to_string() == "10.77.0.1")
    );

    let listener = TcpListener::bind("10.77.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap();
    let url = format!("https://{RELAY_HOST}:{}/", target.port());
    let endpoint_addr = EndpointAddr::new(SecretKey::from_bytes(&[42; 32]).public())
        .with_relay_url(url.parse().unwrap());
    let address = Address::iroh(EndpointTicket::new(endpoint_addr).encode_string());
    let request = SyncRequest::SendEntries(vec![]);
    timeout(WAIT, async {
        tokio::select! {
            accepted = listener.accept() => {
                let (_stream, source) = accepted.unwrap();
                eprintln!("ticket relay {url} resolved to {}: TCP accepted from {source}; dial attempted", target.ip());
            },
            result = transport.send_request(&address, &request) => {
                panic!("request ended before TCP observation: {result:?}");
            }
        }
    }).await.expect("ticket relay hostname must produce an observable private TCP dial attempt");
}
