#![cfg(not(target_os = "windows"))]

use std::collections::BTreeMap;

use anyhow::Ok;
use core_test_support::responses::start_mock_server;
use core_test_support::test_savfox::{TestSavfox, test_savfox};
use core_test_support::{skip_if_no_network, wait_for_event_match};
use pretty_assertions::assert_eq;
use savfox_core::features::Feature;
use savfox_core::protocol::{DeprecationNoticeEvent, EventMsg};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_deprecation_notice_for_legacy_feature_flag() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_savfox().with_config(|config| {
        config.features.enable(Feature::UnifiedExec);
        config
            .features
            .record_legacy_usage_force("use_experimental_unified_exec_tool", Feature::UnifiedExec);
        config.use_experimental_unified_exec_tool = true;
    });

    let TestSavfox { savfox, .. } = builder.build(&server).await?;

    let notice = wait_for_event_match(&savfox, |event| match event {
        EventMsg::DeprecationNotice(ev) => Some(ev.clone()),
        _ => None,
    })
    .await;

    let DeprecationNoticeEvent { summary, details } = notice;
    assert_eq!(
        summary,
        "`use_experimental_unified_exec_tool` is deprecated. Use `[features].unified_exec` instead."
            .to_string(),
    );
    assert_eq!(
        details.as_deref(),
        Some(
            "Enable it with `--enable unified_exec` or `[features].unified_exec` in config.toml. See https://github.com/openai/savfox/blob/main/docs/config.md#feature-flags for details."
        ),
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_deprecation_notice_for_web_search_feature_flags() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_savfox().with_config(|config| {
        let mut entries = BTreeMap::new();
        entries.insert("web_search_request".to_string(), true);
        config.features.apply_map(&entries);
    });

    let TestSavfox { savfox, .. } = builder.build(&server).await?;

    let notice = wait_for_event_match(&savfox, |event| match event {
        EventMsg::DeprecationNotice(ev) if ev.summary.contains("[features].web_search_request") => {
            Some(ev.clone())
        }
        _ => None,
    })
    .await;

    let DeprecationNoticeEvent { summary, details } = notice;
    assert_eq!(
        summary,
        "`[features].web_search_request` is deprecated. Use `web_search` instead.".to_string(),
    );
    assert_eq!(
        details.as_deref(),
        Some(
            "Set `web_search` to `\"live\"`, `\"cached\"`, or `\"disabled\"` at the top level (or under a profile) in config.toml."
        ),
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_deprecation_notice_for_disabled_web_search_feature_flag() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_savfox().with_config(|config| {
        let mut entries = BTreeMap::new();
        entries.insert("web_search_request".to_string(), false);
        config.features.apply_map(&entries);
    });

    let TestSavfox { savfox, .. } = builder.build(&server).await?;

    let notice = wait_for_event_match(&savfox, |event| match event {
        EventMsg::DeprecationNotice(ev) if ev.summary.contains("[features].web_search_request") => {
            Some(ev.clone())
        }
        _ => None,
    })
    .await;

    let DeprecationNoticeEvent { summary, details } = notice;
    assert_eq!(
        summary,
        "`[features].web_search_request` is deprecated. Use `web_search` instead.".to_string(),
    );
    assert_eq!(
        details.as_deref(),
        Some(
            "Set `web_search` to `\"live\"`, `\"cached\"`, or `\"disabled\"` at the top level (or under a profile) in config.toml."
        ),
    );

    Ok(())
}
