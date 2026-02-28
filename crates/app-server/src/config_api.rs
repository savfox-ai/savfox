use std::path::PathBuf;

use savfox_app_server_protocol::{
    ConfigBatchWriteParams, ConfigReadParams, ConfigReadResponse, ConfigRequirements,
    ConfigRequirementsReadResponse, ConfigValueWriteParams, ConfigWriteErrorCode,
    ConfigWriteResponse, JSONRPCErrorError, SandboxMode,
};
use savfox_core::config::{ConfigService, ConfigServiceError};
use savfox_core::config_loader::{
    CloudRequirementsLoader, ConfigRequirementsToml, LoaderOverrides,
    ResidencyRequirement as CoreResidencyRequirement,
    SandboxModeRequirement as CoreSandboxModeRequirement,
};
use serde_json::json;
use toml::Value as TomlValue;

use crate::error_code::{INTERNAL_ERROR_CODE, INVALID_REQUEST_ERROR_CODE};

#[derive(Clone)]
pub(crate) struct ConfigApi {
    service: ConfigService,
}

impl ConfigApi {
    pub(crate) fn new(
        savfox_home: PathBuf,
        cli_overrides: Vec<(String, TomlValue)>,
        loader_overrides: LoaderOverrides,
        cloud_requirements: CloudRequirementsLoader,
    ) -> Self {
        Self {
            service: ConfigService::new(
                savfox_home,
                cli_overrides,
                loader_overrides,
                cloud_requirements,
            ),
        }
    }

    pub(crate) async fn read(
        &self,
        params: ConfigReadParams,
    ) -> Result<ConfigReadResponse, JSONRPCErrorError> {
        self.service.read(params).await.map_err(map_error)
    }

    pub(crate) async fn config_requirements_read(
        &self,
    ) -> Result<ConfigRequirementsReadResponse, JSONRPCErrorError> {
        let requirements = self
            .service
            .read_requirements()
            .await
            .map_err(map_error)?
            .map(map_requirements_toml_to_api);

        Ok(ConfigRequirementsReadResponse { requirements })
    }

    pub(crate) async fn write_value(
        &self,
        params: ConfigValueWriteParams,
    ) -> Result<ConfigWriteResponse, JSONRPCErrorError> {
        self.service.write_value(params).await.map_err(map_error)
    }

    pub(crate) async fn batch_write(
        &self,
        params: ConfigBatchWriteParams,
    ) -> Result<ConfigWriteResponse, JSONRPCErrorError> {
        self.service.batch_write(params).await.map_err(map_error)
    }
}

fn map_requirements_toml_to_api(requirements: ConfigRequirementsToml) -> ConfigRequirements {
    ConfigRequirements {
        allowed_approval_policies: requirements.allowed_approval_policies.map(|policies| {
            policies
                .into_iter()
                .map(savfox_app_server_protocol::AskForApproval::from)
                .collect()
        }),
        allowed_sandbox_modes: requirements.allowed_sandbox_modes.map(|modes| {
            modes
                .into_iter()
                .filter_map(map_sandbox_mode_requirement_to_api)
                .collect()
        }),
        enforce_residency: requirements
            .enforce_residency
            .map(map_residency_requirement_to_api),
    }
}

fn map_sandbox_mode_requirement_to_api(mode: CoreSandboxModeRequirement) -> Option<SandboxMode> {
    match mode {
        CoreSandboxModeRequirement::ReadOnly => Some(SandboxMode::ReadOnly),
        CoreSandboxModeRequirement::WorkspaceWrite => Some(SandboxMode::WorkspaceWrite),
        CoreSandboxModeRequirement::DangerFullAccess => Some(SandboxMode::DangerFullAccess),
        CoreSandboxModeRequirement::ExternalSandbox => None,
    }
}

fn map_residency_requirement_to_api(
    residency: CoreResidencyRequirement,
) -> savfox_app_server_protocol::ResidencyRequirement {
    match residency {
        CoreResidencyRequirement::Us => savfox_app_server_protocol::ResidencyRequirement::Us,
    }
}

fn map_error(err: ConfigServiceError) -> JSONRPCErrorError {
    if let Some(code) = err.write_error_code() {
        return config_write_error(code, err.to_string());
    }

    JSONRPCErrorError {
        code: INTERNAL_ERROR_CODE,
        message: err.to_string(),
        data: None,
    }
}

fn config_write_error(code: ConfigWriteErrorCode, message: impl Into<String>) -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: INVALID_REQUEST_ERROR_CODE,
        message: message.into(),
        data: Some(json!({
            "config_write_error_code": code,
        })),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use savfox_protocol::protocol::AskForApproval as CoreAskForApproval;

    use super::*;

    #[test]
    fn map_requirements_toml_to_api_converts_core_enums() {
        let requirements = ConfigRequirementsToml {
            allowed_approval_policies: Some(vec![
                CoreAskForApproval::Never,
                CoreAskForApproval::OnRequest,
            ]),
            allowed_sandbox_modes: Some(vec![
                CoreSandboxModeRequirement::ReadOnly,
                CoreSandboxModeRequirement::ExternalSandbox,
            ]),
            mcp_servers: None,
            rules: None,
            enforce_residency: Some(CoreResidencyRequirement::Us),
        };

        let mapped = map_requirements_toml_to_api(requirements);

        assert_eq!(
            mapped.allowed_approval_policies,
            Some(vec![
                savfox_app_server_protocol::AskForApproval::Never,
                savfox_app_server_protocol::AskForApproval::OnRequest,
            ])
        );
        assert_eq!(
            mapped.allowed_sandbox_modes,
            Some(vec![SandboxMode::ReadOnly]),
        );
        assert_eq!(
            mapped.enforce_residency,
            Some(savfox_app_server_protocol::ResidencyRequirement::Us),
        );
    }
}
