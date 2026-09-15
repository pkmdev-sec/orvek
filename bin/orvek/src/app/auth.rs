//! Authentication selection and shared ChatGPT credential management.

use crate::app::{
    config::{AuthConfig, AuthMode},
    error::{AuthError, AuthResult, SecretError},
    secret::SecretString,
};
use orvek_harness::{
    Digest,
    inference::auth::{
        Auth, ChatGptAuthStatus, ChatGptLogin, SecretString as ProviderSecret, chatgpt_auth_status,
        logout_chatgpt,
    },
};
use std::{path::Path, result::Result as StdResult};

enum SelectedAuth {
    ChatGpt,
    ApiKey(SecretString),
}

impl AuthConfig {
    pub(crate) async fn login(&self) -> AuthResult<()> {
        let login = ChatGptLogin::start(self.file()).await?;

        eprintln!(
            "Open this URL to sign in with ChatGPT:\n\n{}\n",
            login.authorization_url()
        );
        if let Err(error) = crate::app::browser::open(login.authorization_url()) {
            eprintln!(
                "Could not open a browser automatically ({error}). Open the URL above manually."
            );
        }

        let account = login.complete().await?;
        eprintln!("{}", self.login_success(&account));
        Ok(())
    }

    pub(crate) fn load(&self) -> AuthResult<Auth> {
        let selected = self.select_auth(|| SecretString::from_environment(self.api_key_env()))?;

        selected.into_provider_auth(self.file())
    }

    pub(crate) fn credential_identity(&self) -> AuthResult<Option<Digest>> {
        let selected = self.select_auth(|| SecretString::from_environment(self.api_key_env()))?;

        Ok(selected.credential_identity())
    }

    pub(crate) fn status(&self) -> AuthResult<()> {
        let api_key_env = self.api_key_env();
        match self.select_auth(|| SecretString::from_environment(api_key_env))? {
            SelectedAuth::ChatGpt => self.print_chatgpt_status()?,
            SelectedAuth::ApiKey(_api_key) => {
                println!("Authentication: API key from {api_key_env}");
            }
        }

        Ok(())
    }

    pub(crate) fn logout(&self) -> AuthResult<()> {
        if logout_chatgpt(self.file())? {
            eprintln!(
                "Removed shared ChatGPT credentials from {}. Orvek and Codex are logged out.",
                self.file().display()
            );
            return Ok(());
        }

        eprintln!(
            "No ChatGPT credentials were stored at {}.",
            self.file().display()
        );
        Ok(())
    }

    fn select_auth<F>(&self, read_api_key: F) -> AuthResult<SelectedAuth>
    where
        F: FnOnce() -> StdResult<Option<SecretString>, SecretError>,
    {
        match self.mode() {
            AuthMode::ChatGpt => Ok(SelectedAuth::ChatGpt),
            AuthMode::ApiKey => read_api_key()?
                .map(SelectedAuth::ApiKey)
                .ok_or(AuthError::ApiKeyUnavailable),
            AuthMode::Auto => {
                if self
                    .file()
                    .try_exists()
                    .map_err(|source| AuthError::InspectCredentialFile {
                        path: self.file().to_path_buf(),
                        source,
                    })?
                {
                    return Ok(SelectedAuth::ChatGpt);
                }

                read_api_key()?.map(SelectedAuth::ApiKey).ok_or_else(|| {
                    AuthError::CredentialsUnavailable {
                        path: self.file().to_path_buf(),
                    }
                })
            }
        }
    }

    fn print_chatgpt_status(&self) -> AuthResult<()> {
        let account = chatgpt_auth_status(self.file())?;
        println!("Authentication: ChatGPT");
        if let Some(email) = account.email {
            println!("Email: {email}");
        }
        if let Some(plan) = account.plan {
            println!("Plan: {plan}");
        }
        println!("Account: {}", account.account_id);
        println!("FedRAMP: {}", account.fedramp);
        println!("Credentials: {}", self.file().display());
        Ok(())
    }

    fn login_success(&self, account: &ChatGptAuthStatus) -> String {
        let identity = account
            .email
            .as_deref()
            .map_or(String::new(), |email| format!(" as {email}"));
        format!(
            "Orvek and Codex are logged in{identity} (account {}). Credentials saved to {}.",
            account.account_id,
            self.file().display()
        )
    }
}

impl SelectedAuth {
    fn credential_identity(&self) -> Option<Digest> {
        match self {
            Self::ChatGpt => None,
            Self::ApiKey(api_key) => Some(Digest::of(api_key.expose_secret().as_bytes())),
        }
    }

    fn into_provider_auth(self, auth_file: &Path) -> AuthResult<Auth> {
        match self {
            Self::ChatGpt => Auth::chatgpt(auth_file.to_owned()).map_err(Into::into),
            Self::ApiKey(api_key) => {
                Auth::api_key(ProviderSecret::new(api_key.expose_secret().to_owned()))
                    .map_err(Into::into)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SelectedAuth;
    use crate::app::{
        config::{AuthConfig, AuthMode},
        error::AuthError,
        secret::SecretString,
    };
    use orvek_harness::inference::auth::AuthMode as ProviderAuthMode;
    use std::{cell::Cell, fs};
    use tempfile::tempdir;

    #[test]
    fn auto_prefers_an_existing_chatgpt_file_without_reading_the_api_key() {
        let directory = tempdir().unwrap();
        let auth_file = directory.path().join("auth.json");
        fs::write(&auth_file, "invalid but present").unwrap();
        let api_key_read = Cell::new(false);

        let config = AuthConfig::new(AuthMode::Auto, auth_file, None);
        let selected = config
            .select_auth(|| {
                api_key_read.set(true);
                Ok(Some(SecretString::new("api-key".into())))
            })
            .unwrap();

        assert!(matches!(selected, SelectedAuth::ChatGpt));
        assert!(!api_key_read.get());
    }

    #[test]
    fn auto_falls_back_to_an_api_key_when_chatgpt_is_absent() {
        let directory = tempdir().unwrap();
        let config = AuthConfig::new(AuthMode::Auto, directory.path().join("auth.json"), None);
        let selected = config
            .select_auth(|| Ok(Some(SecretString::new("api-key".into()))))
            .unwrap();

        assert!(matches!(selected, SelectedAuth::ApiKey(_)));
    }

    #[test]
    fn forced_chatgpt_does_not_read_the_api_key() {
        let api_key_read = Cell::new(false);
        let config = AuthConfig::new(AuthMode::ChatGpt, "missing.json".into(), None);
        let selected = config
            .select_auth(|| {
                api_key_read.set(true);
                Ok(Some(SecretString::new("api-key".into())))
            })
            .unwrap();

        assert!(matches!(selected, SelectedAuth::ChatGpt));
        assert!(!api_key_read.get());
    }

    #[test]
    fn forced_api_key_reports_a_missing_environment_value() {
        let config = AuthConfig::new(AuthMode::ApiKey, "unused.json".into(), None);
        let result = config.select_auth(|| Ok(None));

        assert!(matches!(result, Err(AuthError::ApiKeyUnavailable)));
    }

    #[test]
    fn selected_api_key_constructs_redacted_native_authorization() {
        let selected = SelectedAuth::ApiKey(SecretString::new("api-key".into()));
        let auth = selected.into_provider_auth("unused.json".as_ref()).unwrap();

        assert_eq!(auth.mode(), ProviderAuthMode::ApiKey);
        assert!(!format!("{auth:?}").contains("api-key"));
    }

    #[test]
    fn api_key_identity_detects_rotation_without_retaining_the_key() {
        let first = SelectedAuth::ApiKey(SecretString::new("first-api-key".into()));
        let same = SelectedAuth::ApiKey(SecretString::new("first-api-key".into()));
        let rotated = SelectedAuth::ApiKey(SecretString::new("rotated-api-key".into()));

        assert_eq!(first.credential_identity(), same.credential_identity());
        assert_ne!(first.credential_identity(), rotated.credential_identity());
        assert_eq!(SelectedAuth::ChatGpt.credential_identity(), None);
        let rendered = format!("{:?}", first.credential_identity());
        assert!(!rendered.contains("first-api-key"));
    }

    #[test]
    fn logout_is_idempotent() {
        let directory = tempdir().unwrap();
        let auth_file = directory.path().join("auth.json");
        fs::write(&auth_file, "credentials").unwrap();
        let config = AuthConfig::new(AuthMode::ChatGpt, auth_file.clone(), None);

        config.logout().unwrap();
        assert!(!auth_file.exists());
        config.logout().unwrap();
    }
}
