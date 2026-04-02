use std::fmt;

use savfox_utils::absolute_path::AbsolutePathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementSource {
    Unknown,
    MdmManagedPreferences { domain: String, key: String },
    CloudRequirements,
    SystemRequirementsToml { file: AbsolutePathBuf },
}

impl fmt::Display for RequirementSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "<unspecified>"),
            Self::MdmManagedPreferences { domain, key } => {
                write!(f, "MDM {domain}:{key}")
            }
            Self::CloudRequirements => {
                write!(f, "cloud requirements")
            }
            Self::SystemRequirementsToml { file } => {
                write!(f, "{}", file.as_path().display())
            }
        }
    }
}
