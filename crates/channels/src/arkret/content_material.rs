//! Preserve the signed original until native root and endpoint material can be
//! resolved. Parsing an envelope alone never produces an authenticated header.

use anyhow::Context as _;
use arkret::{
    EncryptedEnvelope, EncryptedPayload, EncryptedPayloadScheme, Event, EventContentRoutingContext,
    Hash, MlsGroupId,
};
use arkret_models_crypto::EventContentMaterial;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArkretEventContent {
    pub original: Event,
    pub envelope: EncryptedEnvelope,
    pub group_id: MlsGroupId,
    pub epoch: u64,
    pub payload_digest: Hash,
    pub content_type: String,
    pub scheme: EncryptedPayloadScheme,
}

impl ArkretEventContent {
    pub(crate) fn from_original(
        original: &Event,
        envelope: &EncryptedEnvelope,
    ) -> anyhow::Result<Self> {
        envelope.validate()?;
        Ok(Self {
            original: original.clone(),
            envelope: envelope.clone(),
            group_id: original.scope_ref.canonical_mls_group_id()?,
            epoch: envelope.encryption_context.epoch(),
            payload_digest: Hash::new(arkret::canonical::canonical_sha256(&(original, envelope))?)?,
            content_type: envelope.content_type.clone(),
            scheme: envelope.encryption_scheme.clone(),
        })
    }

    pub(crate) fn resolve(
        &self,
        material: EventContentMaterial,
    ) -> anyhow::Result<EncryptedPayload> {
        anyhow::ensure!(
            material.outer_event == EventContentMaterial::outer_event_for(&self.original)?,
            "content material differs from its original outer Event"
        );
        let sender = arkret::mls_basic_credential_identity(self.original.actual_signer())?;
        let header = self.envelope.reconstruct_pre_encryption_header(
            self.scheme.clone(),
            self.original.scope_ref.clone(),
            self.original.kind.as_str(),
            std::str::from_utf8(&sender).context("content sender credential is not UTF-8")?,
            None,
            material,
        )?;
        Ok(
            arkret::mls::encrypted_envelope_to_payload_with_verified_header(
                &self.envelope,
                header,
            )?,
        )
    }
}

pub(super) fn salt_binding(
    event: &Event,
    path: &str,
    mime: &str,
    plaintext: &[u8],
    root_context: &arkret_models_crypto::ContentRootContext,
    root_ref: &Hash,
    producer: &arkret_models_crypto::ContentEndpoint,
    protection: &arkret_models_crypto::ContentProtection,
) -> anyhow::Result<Hash> {
    Ok(Hash::new(arkret::canonical::canonical_sha256(
        &serde_json::json!({
            "outer_event":EventContentMaterial::outer_event_for(event)?,"content_path":path,"content_type":mime,
            "plaintext_digest":arkret::canonical::sha256_digest(plaintext),
            "root_context":root_context,"root_ref":root_ref,
            "producer":producer,"protection":protection,
            "routing_context":EventContentRoutingContext::None,
        }),
    )?)?)
}
