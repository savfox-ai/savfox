use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct WizardStatus {
    pub state: Option<String>,
    pub current_step: Option<u32>,
    pub total_steps: Option<u32>,
    pub step_title: Option<String>,
    pub step_description: Option<String>,
}
