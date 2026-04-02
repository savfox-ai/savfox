use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use futures::FutureExt as _;
use rmcp::model::{
    AnnotateAble, ClientCapabilities, Implementation, InitializeRequestParams,
    ListResourceTemplatesResult, ProtocolVersion, ReadResourceRequestParams, ResourceContents,
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
                    Ok(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: Some(json!({})),
                    })
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
        &rmcp::model::RawResource {
            uri: RESOURCE_URI.to_owned(),
            name: "example-note".to_owned(),
            title: Some("Example Note".to_owned()),
            description: Some("A sample MCP resource exposed for integration tests.".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            size: None,
            icons: None,
            meta: None,
        }
        .no_annotation()
    );
    let templates = client
        .list_resource_templates(None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(
        templates,
        ListResourceTemplatesResult {
            meta: None,
            next_cursor: None,
            resource_templates: vec![
                rmcp::model::RawResourceTemplate {
                    uri_template: "memo://savfox/{slug}".to_owned(),
                    name: "savfox-memo".to_owned(),
                    title: Some("Savfox Memo".to_owned()),
                    description: Some(
                        "Template for memo://savfox/{slug} resources used in tests.".to_owned(),
                    ),
                    mime_type: Some("text/plain".to_owned()),
                    icons: None,
                }
                .no_annotation()
            ],
        }
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
