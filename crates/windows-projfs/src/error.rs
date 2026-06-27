use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Possible errors which can occurr while library usage.
#[derive(Debug, Error)]
pub enum Error {
    /// A generic error occurred within the underlying Windows API
    #[error("{0}")]
    GenericWindows(#[from] windows::core::Error),

    /// Failed to create the projection root directory
    #[error("failed to mark projection root: {0}")]
    MarkProjectionRoot(windows::core::Error),

    /// Failed to start the projection
    #[error("failed to start projection: {0}")]
    StartProjection(windows::core::Error),

    /// The Windows feature "Projected File System" is not enabled.
    /// This feature has to be enabled before using this library.
    ///
    /// Note:
    /// This error can only occurr with feature "dynamic-import" else
    /// you would receive a DLL loading error when starting your application.
    #[error("The Windows feature \"Projected File System\" is not enabled")]
    WindowsFeatureNotEnabled,

    /// Failed to resolve certain Windows project fs API imports
    /// which are required for this library to work.
    #[cfg(feature = "dynamic-import")]
    #[error("failed to resolve imports: {0}")]
    LibraryError(#[from] libloading::Error),
}
