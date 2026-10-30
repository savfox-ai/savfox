//! Carry policy-checked DNS candidates to the transport without a second lookup.

use std::net::SocketAddr;

use rama_core::Service;
use rama_core::error::{BoxError, BoxErrorExt as _};
use rama_core::extensions::{Extension, Extensions};
use rama_core::futures::stream;
use rama_core::futures::stream::BoxStream;
use rama_dns::client::DnsConnector;
use rama_net::client::{
    AddressCandidates, ConnectionError, ConnectionErrorKind, ConnectorTargetStream,
    EstablishedClientConnection, ProxyRoute,
};
use rama_net::{ConnectorTargetInputExt, TransportProtocolInputExt};
use rama_tcp::TcpStream;
use rama_tcp::client::service::TcpConnector;

use crate::policy::normalize_host;

#[derive(Debug, Clone, Extension)]
pub(crate) struct ValidatedTarget {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) addresses: Vec<SocketAddr>,
}

struct PinnedCandidates(Vec<SocketAddr>);

impl AddressCandidates for PinnedCandidates {
    fn stream<'a>(
        &'a self,
        _extensions: &'a Extensions,
    ) -> BoxStream<'a, Result<SocketAddr, BoxError>> {
        Box::pin(stream::iter(self.0.iter().copied().map(Ok)))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PolicyConnector;

impl<Input> Service<Input> for PolicyConnector
where
    Input: ConnectorTargetInputExt + TransportProtocolInputExt + Send + 'static,
{
    type Output = EstablishedClientConnection<TcpStream, Input>;
    type Error = ConnectionError;

    async fn serve(&self, input: Input) -> Result<Self::Output, Self::Error> {
        let target = input.connector_target().ok_or_else(|| {
            ConnectionError::local(
                BoxError::from_static_str("network proxy: connector target missing"),
                ConnectionErrorKind::InvalidInput,
            )
        })?;
        if let Some(validated) = input.extensions().get_ref::<ValidatedTarget>() {
            if validated.host == normalize_host(&target.host.to_string())
                && validated.port == target.port
            {
                if validated.addresses.is_empty() {
                    return Err(ConnectionError::local(
                        BoxError::from_static_str("network proxy: validated address list is empty"),
                        ConnectionErrorKind::InvalidInput,
                    ));
                }
                input
                    .extensions()
                    .insert(ConnectorTargetStream::new(PinnedCandidates(
                        validated.addresses.clone(),
                    )));
                return TcpConnector::new().serve(input).await;
            }
            // The upstream proxy is configured by the operator, and is a
            // different network hop from the policy-checked destination.
            let trusted_proxy_hop = input
                .extensions()
                .get_ref::<ProxyRoute>()
                .and_then(ProxyRoute::proxy_address)
                .is_some_and(|proxy| proxy.address == target);
            if !trusted_proxy_hop {
                return Err(ConnectionError::local(
                    BoxError::from_static_str(
                        "network proxy: target differs from the checked authority",
                    ),
                    ConnectionErrorKind::InvalidInput,
                ));
            }
        }
        // Local-network opt-in and explicitly permitted local literals do not
        // undergo the public-address check; resolve those domains once here.
        DnsConnector::new(TcpConnector::new()).serve(input).await
    }
}

#[cfg(test)]
mod tests {
    use rama_net::client::ConnectRequest;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn checked_candidates_are_used_without_resolving_the_hostname_again() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"pinned").await.unwrap();
        });
        // A fake policy result isolates the transport contract: this hostname
        // cannot resolve, so the test can only pass using the supplied candidate.
        let input = ConnectRequest::new(
            format!("checked.invalid:{}", address.port())
                .parse()
                .unwrap(),
        );
        input.extensions.insert(ValidatedTarget {
            host: "checked.invalid".to_owned(),
            port: address.port(),
            addresses: vec![address],
        });
        let mut connected = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            PolicyConnector.serve(input),
        )
        .await
        .unwrap()
        .unwrap()
        .conn;
        let mut bytes = [0; 6];
        connected.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pinned");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_changed_authority_cannot_reuse_another_hosts_checked_addresses() {
        for authority in ["other.invalid:443", "checked.invalid:8443"] {
            let input = ConnectRequest::new(authority.parse().unwrap());
            input.extensions.insert(ValidatedTarget {
                host: "checked.invalid".to_owned(),
                port: 443,
                addresses: vec!["127.0.0.1:443".parse().unwrap()],
            });
            let error = PolicyConnector.serve(input).await.unwrap_err();
            assert!(error.to_string().contains("checked authority"));
        }
    }

    #[tokio::test]
    async fn a_proxy_route_cannot_authorize_a_different_connection_target() {
        use rama_net::client::ConnectorTarget;

        let input = ConnectRequest::new("checked.invalid:443".parse().unwrap());
        input.extensions.insert(ValidatedTarget {
            host: "checked.invalid".to_owned(),
            port: 443,
            addresses: vec!["8.8.8.8:443".parse().unwrap()],
        });
        input
            .extensions
            .insert(ProxyRoute::Proxy("http://127.0.0.1:9".parse().unwrap()));
        input
            .extensions
            .insert(ConnectorTarget("127.0.0.1:10".parse().unwrap()));
        let error = PolicyConnector.serve(input).await.unwrap_err();
        assert!(error.to_string().contains("checked authority"));
    }
}
