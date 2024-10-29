use shadow_rs::{
    SdResult, BUILD_OS, BUILD_TARGET, BUILD_TARGET_ARCH, CARGO_MANIFEST_DIR, CARGO_METADATA,
    CARGO_TREE, CARGO_VERSION, COMMIT_AUTHOR, COMMIT_DATE, COMMIT_DATE_2822, COMMIT_DATE_3339,
    COMMIT_EMAIL, COMMIT_HASH, GIT_CLEAN, GIT_STATUS_FILE, LAST_TAG, PKG_DESCRIPTION,
    PKG_VERSION_MAJOR, PKG_VERSION_MINOR, PKG_VERSION_PATCH, PKG_VERSION_PRE, TAG,
};

fn main() -> SdResult<()> {
    shadow_rs::new_deny(
        [
            BUILD_OS,
            BUILD_TARGET,
            BUILD_TARGET_ARCH,
            CARGO_MANIFEST_DIR,
            CARGO_METADATA,
            CARGO_TREE,
            CARGO_VERSION,
            COMMIT_AUTHOR,
            COMMIT_DATE,
            COMMIT_DATE_2822,
            COMMIT_DATE_3339,
            COMMIT_EMAIL,
            COMMIT_HASH,
            GIT_CLEAN,
            GIT_STATUS_FILE,
            LAST_TAG,
            PKG_DESCRIPTION,
            PKG_VERSION_MAJOR,
            PKG_VERSION_MINOR,
            PKG_VERSION_PATCH,
            PKG_VERSION_PRE,
            TAG,
            "BUILD_TIME_2822",
            "BUILD_RUST_CHANNEL",
            "PROJECT_NAME",
            // Required for undeniable `CLAP_LONG_VERSION`.

            // BRANCH,
            // PKG_VERSION,
            // RUST_CHANNEL,
            // "BUILD_TIME",
        ]
        .into(),
    )
}
