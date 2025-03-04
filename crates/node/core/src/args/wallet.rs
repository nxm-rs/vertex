use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::{LocalSigner, PrivateKeySigner};
use clap::{command, Args};
use std::{path::PathBuf, str::FromStr, sync::Arc};

#[derive(Debug, Clone, Args, PartialEq, Eq)]
#[command(next_help_heading = "Wallet Configuration")]
pub struct WalletArgs {
    /// The path to the JSON keystore file
    #[arg(
        long,
        value_name = "PATH",
        requires = "password",
        group = "wallet_config"
    )]
    pub keystore_file: Option<PathBuf>,

    /// The password to unlock the keystore file
    #[arg(
        long,
        value_name = "PASSWORD",
        requires = "keystore_file",
        group = "wallet_config"
    )]
    pub password: Option<String>,

    /// The raw private key to use for the wallet as a hex string
    #[arg(long, value_name = "PRIVATE_KEY", group = "wallet_config")]
    pub private_key: Option<String>,
}

impl WalletArgs {
    /// Returns the signer in an Arc
    pub fn signer(&self) -> Arc<LocalSigner<SigningKey>> {
        match (self.keystore_file.as_ref(), self.private_key.as_ref()) {
            (Some(keystore_file), None) => {
                // We can safely unwrap password here because of the requires attribute
                Arc::new(
                    LocalSigner::decrypt_keystore(keystore_file, self.password.as_ref().unwrap())
                        .unwrap(),
                )
            }
            (None, Some(private_key)) => Arc::new(PrivateKeySigner::from_str(private_key).unwrap()),
            _ => Arc::new(PrivateKeySigner::random()),
        }
    }
}
