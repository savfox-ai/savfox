use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use futures::FutureExt as _;
use rmcp::model::{
    ClientCapabilities, Implementation, InitializeRequestParams, ProtocolVersion,
    ReadResourceRequestParams, Resource, ResourceContents, ResourceTemplate,
};
use savfox_rmcp_client::{ElicitationAction, ElicitationResponse, RmcpClient};
use savfox_utils::cargo_bin::CargoBinError;
use serde_json::json;

const RESOURCE_URI: &str = "memo://savfox/example-note";

fn stdio_server_bin() -> Result<PathBuf, CargoBinError> {
    savfox_utils::cargo_bin::cargo_bin("test_stdio_server")
}

fn init_params() -> InitializeRequestParams {
    let capabilities = ClientCapabilities::builder().enable_elicitation().build();
    let client_info =
        Implementation::new("savfox-test", "0.0.0-test").with_title("Savfox rmcp resource test");
    InitializeRequestParams::new(capabilities, client_info)
        .with_protocol_version(ProtocolVersion::V_2025_06_18)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn default_sdk_version_still_uses_the_initialize_lifecycle() -> anyhow::Result<()> {
    let client =
        RmcpClient::new_stdio_client(stdio_server_bin()?.into(), Vec::new(), None, &[], None)
            .await?;
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("savfox-test", "test"),
    );
    assert!(
        !params.protocol_version.has_initialize(),
        "the SDK default should exercise the handshake-free revision"
    );
    let initialized = client
        .initialize(
            params,
            Some(Duration::from_secs(5)),
            Box::new(|_, _| {
                async { Ok(ElicitationResponse::new(ElicitationAction::Cancel)) }.boxed()
            }),
        )
        .await?;
    assert!(initialized.protocol_version.has_initialize());
    assert!(initialized.capabilities.tools.is_some());
    assert!(!initialized.server_info.name.is_empty());
    assert!(
        !client
            .list_tools(None, Some(Duration::from_secs(5)))
            .await?
            .tools
            .is_empty()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rmcp_client_can_list_and_read_resources() -> anyhow::Result<()> {
    let client = RmcpClient::new_stdio_client(
        stdio_server_bin()?.into(),
        Vec::<OsString>::new(),
        None,
        &[],
        None,
    )
    .await?;

    client
        .initialize(
            init_params(),
            Some(Duration::from_secs(5)),
            Box::new(|_, _| {
                async {
                    Ok(ElicitationResponse::new(ElicitationAction::Accept).with_content(json!({})))
                }
                .boxed()
            }),
        )
        .await?;

    let list = client
        .list_resources(None, Some(Duration::from_secs(5)))
        .await?;
    let memo = list
        .resources
        .iter()
        .find(|resource| resource.uri == RESOURCE_URI)
        .expect("memo resource present");
    assert_eq!(
        memo,
        &Resource::new(RESOURCE_URI, "example-note")
            .with_title("Example Note")
            .with_description("A sample MCP resource exposed for integration tests.")
            .with_mime_type("text/plain")
    );
    let templates = client
        .list_resource_templates(None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(
        templates.resource_templates,
        vec![
            ResourceTemplate::new("memo://savfox/{slug}", "savfox-memo")
                .with_title("Savfox Memo")
                .with_description("Template for memo://savfox/{slug} resources used in tests.")
                .with_mime_type("text/plain")
        ]
    );

    let read = client
        .read_resource(
            ReadResourceRequestParams::new(RESOURCE_URI),
            Some(Duration::from_secs(5)),
        )
        .await?;
    let text = read.contents.first().expect("resource contents present");
    assert_eq!(
        text,
        &ResourceContents::TextResourceContents {
            uri: RESOURCE_URI.to_owned(),
            mime_type: Some("text/plain".to_owned()),
            text: "This is a sample MCP resource served by the rmcp test server.".to_owned(),
            meta: None,
        }
    );

    Ok(())
}
