#![cfg(not(target_os = "windows"))]

use std::collections::BTreeMap;

use anyhow::Ok;
use core_test_support::responses::start_mock_server;
use core_test_support::test_savfox::{TestSavfox, test_savfox};
use core_test_support::{skip_if_no_network, test_absolute_path, wait_for_event_match};
use savfox_app_server_protocol::ConfigLayerSource;
use savfox_core::config_loader::{
    ConfigLayerEntry, ConfigLayerStack, ConfigRequirements, ConfigRequirementsToml,
};
use savfox_core::features::Feature;
use savfox_core::protocol::{DeprecationNoticeEvent, EventMsg};
use pretty_assertions::assert_eq;
use toml::Value as TomlValue;

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
async fn emits_deprecation_notice_for_experimental_instructions_file() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_savfox().with_config(|config| {
        let mut table = toml::map::Map::new();
        table.insert(
            "experimental_instructions_file".to_string(),
            TomlValue::String("legacy.md".to_string()),
        );
        let config_layer = ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: test_absolute_path("/tmp/config.toml"),
            },
            TomlValue::Table(table),
        );
        let config_layer_stack = ConfigLayerStack::new(
            vec![config_layer],
            ConfigRequirements::default(),
            ConfigRequirementsToml::default(),
        )
        .expect("build config layer stack");
        config.config_layer_stack = config_layer_stack;
    });

    let TestSavfox { savfox, .. } = builder.build(&server).await?;

    let notice = wait_for_event_match(&savfox, |event| match event {
        EventMsg::DeprecationNotice(ev)
            if ev.summary.contains("experimental_instructions_file") =>
        {
            Some(ev.clone())
        }
        _ => None,
    })
    .await;

    let DeprecationNoticeEvent { summary, details } = notice;
    assert_eq!(
        summary,
        "`experimental_instructions_file` is deprecated and ignored. Use `model_instructions_file` instead."
            .to_string(),
    );
    assert_eq!(
        details.as_deref(),
        Some(
            "Move the setting to `model_instructions_file` in config.toml (or under a profile) to load instructions from a file."
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
