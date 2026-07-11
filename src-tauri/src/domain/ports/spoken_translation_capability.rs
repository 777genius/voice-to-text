#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenIncomingCapability {
    Ready,
    UnsupportedPlatform,
    PermissionRequired,
    UnsafeSelfCapture,
    NoOutputDevice,
    UnsupportedTargetLanguage,
}

pub trait SpokenTranslationCapability: Send + Sync {
    fn check(&self, target_language: &str) -> SpokenIncomingCapability;
}
