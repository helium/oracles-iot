use std::net::{SocketAddr, TcpListener};

use backon::{ExponentialBuilder, Retryable};
use helium_crypto::{KeyTag, Keypair, Network, PublicKey, Sign};
use helium_proto::services::poc_lora::{
    lora_stream_request_v1::Request as StreamRequest,
    lora_stream_response_v1::Response as StreamResponse, poc_lora_client::PocLoraClient,
    LoraBeaconReportReqV1, LoraStreamRequestV1, LoraStreamResponseV1, LoraStreamSessionInitV1,
    LoraStreamSessionOfferV1, LoraWitnessReportReqV1,
};
use ingest::server_iot::GrpcServer;
use prost::Message;
use rand::rngs::OsRng;
use task_manager::TaskManager;
use tokio::{task::LocalSet, time::timeout};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{transport::Channel, Streaming};

/// Beacon/witness data is now dropped on the floor (POC retired). These tests
/// verify the session-management mechanics still work correctly: sessions open
/// and close as expected, and bad signatures / wrong pubkeys terminate the stream.

#[tokio::test]
async fn initialize_session_and_send_beacon_and_witness() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Beacons and witnesses are silently dropped; stream remains open.
            client.send_beacon(pub_key.public_key(), &session_key).await;
            client
                .send_witness(pub_key.public_key(), &session_key)
                .await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_after_incorrectly_signed_init_request() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    // should be signed by pub_key
                    &session_key,
                )
                .await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_after_incorrectly_signed_beacon() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Incorrectly signed by pub_key
            client.send_beacon(pub_key.public_key(), &pub_key).await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_stops_after_incorrect_beacon_pubkey() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Incorrect pub_key sent
            let other_key = generate_keypair();
            client
                .send_beacon(other_key.public_key(), &session_key)
                .await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_after_incorrectly_signed_witness() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Incorrectly signed by pub_key
            client.send_witness(pub_key.public_key(), &pub_key).await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_after_incorrect_witness_pubkey() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Incorrect pub_key
            let other_key = generate_keypair();
            client
                .send_witness(other_key.public_key(), &session_key)
                .await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stop_if_client_attempts_to_initialize_2nd_session() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, None, None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            client
                .send_init(
                    offer.clone(),
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Beacon is silently dropped; stream remains open.
            client.send_beacon(pub_key.public_key(), &session_key).await;

            // Attempting a second session init should close the stream.
            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_if_init_not_sent_within_timeout() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, Some(500), None);
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let mut client = connect_and_stream(addr).await;
            let _offer = client.receive_offer().await;

            client.assert_closed().await;
        })
        .await;
}

#[tokio::test]
async fn stream_stops_on_session_timeout() {
    let addr = get_socket_addr().expect("socket addr");

    LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let server = create_test_server(addr, Some(500), Some(900));
                TaskManager::builder()
                    .add_task(server)
                    .build()
                    .start()
                    .await
            });

            let mut client = connect_and_stream(addr).await;
            let offer = client.receive_offer().await;

            let pub_key = generate_keypair();
            let session_key = generate_keypair();

            client
                .send_init(
                    offer,
                    pub_key.public_key(),
                    session_key.public_key(),
                    &pub_key,
                )
                .await;

            // Beacon silently dropped; session timeout will close the stream.
            client.send_beacon(pub_key.public_key(), &session_key).await;

            client.assert_closed().await;
        })
        .await;
}

async fn connect_and_stream(socket_addr: SocketAddr) -> TestClient {
    let mut client = (|| PocLoraClient::connect(format!("http://{socket_addr}")))
        .retry(&ExponentialBuilder::default())
        .await
        .expect("client connect");

    let (stream_tx, stream_rx) = tokio::sync::mpsc::channel(5);
    let response = client
        .stream_requests(ReceiverStream::new(stream_rx))
        .await
        .expect("stream requests");

    TestClient {
        _client: client,
        stream_tx,
        in_stream: response.into_inner(),
    }
}

struct TestClient {
    _client: PocLoraClient<Channel>,
    stream_tx: tokio::sync::mpsc::Sender<LoraStreamRequestV1>,
    in_stream: Streaming<LoraStreamResponseV1>,
}

impl TestClient {
    async fn receive_offer(&mut self) -> LoraStreamSessionOfferV1 {
        match timeout(seconds(1), self.in_stream.next()).await {
            Ok(Some(Ok(LoraStreamResponseV1 {
                response: Some(StreamResponse::Offer(offer)),
            }))) => offer,
            Ok(None) => panic!("server closed stream waiting for offer"),
            Ok(_) => panic!("invalid offer received"),
            Err(_) => panic!("timeout exceeded waiting for offer"),
        }
    }

    async fn assert_closed(mut self) {
        let Ok(None) = timeout(seconds(1), self.in_stream.next()).await else {
            panic!("Should have received None to indicate server closed connection")
        };
    }

    async fn send_init(
        &mut self,
        offer: LoraStreamSessionOfferV1,
        pub_key: &PublicKey,
        session_key: &PublicKey,
        signing_key: &Keypair,
    ) {
        let mut init = LoraStreamSessionInitV1 {
            pub_key: pub_key.into(),
            nonce: offer.nonce,
            session_key: session_key.into(),
            signature: vec![],
        };

        init.signature = signing_key.sign(&init.encode_to_vec()).expect("sign");

        let request = LoraStreamRequestV1 {
            request: Some(StreamRequest::SessionInit(init)),
        };

        self.stream_tx
            .send(request)
            .await
            .expect("send init failed");
    }

    async fn send_beacon(&mut self, pub_key: &PublicKey, signing_key: &Keypair) {
        let mut report = LoraBeaconReportReqV1 {
            pub_key: pub_key.into(),
            local_entropy: vec![],
            remote_entropy: vec![],
            data: vec![],
            frequency: 0,
            channel: 0,
            datarate: 0,
            tx_power: 0,
            timestamp: 0,
            signature: vec![],
            tmst: 0,
        };

        report.signature = signing_key.sign(&report.encode_to_vec()).expect("sign");

        let request = LoraStreamRequestV1 {
            request: Some(StreamRequest::BeaconReport(report)),
        };

        self.stream_tx
            .send(request)
            .await
            .expect("send beacon failed");
    }

    async fn send_witness(&mut self, pub_key: &PublicKey, signing_key: &Keypair) {
        let mut report = LoraWitnessReportReqV1 {
            pub_key: pub_key.into(),
            data: vec![],
            timestamp: 0,
            tmst: 0,
            signal: 0,
            snr: 0,
            frequency: 0,
            datarate: 0,
            signature: vec![],
        };

        report.signature = signing_key.sign(&report.encode_to_vec()).expect("sign");

        let request = LoraStreamRequestV1 {
            request: Some(StreamRequest::WitnessReport(report)),
        };

        self.stream_tx
            .send(request)
            .await
            .expect("send witness failed");
    }
}

fn create_test_server(
    socket_addr: SocketAddr,
    offer_timeout: Option<u64>,
    timeout: Option<u64>,
) -> GrpcServer {
    let offer_timeout = offer_timeout.unwrap_or(5000);
    let timeout = timeout.unwrap_or(30 * 60000);
    GrpcServer {
        required_network: Network::MainNet,
        address: socket_addr,
        session_key_offer_timeout: std::time::Duration::from_millis(offer_timeout),
        session_key_timeout: std::time::Duration::from_millis(timeout),
    }
}

fn generate_keypair() -> Keypair {
    Keypair::generate(KeyTag::default(), &mut OsRng)
}

fn seconds(s: u64) -> std::time::Duration {
    std::time::Duration::from_secs(s)
}

fn get_socket_addr() -> anyhow::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?)
}
