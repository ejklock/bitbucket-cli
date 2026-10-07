use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Select};

use tabled::{Table, Tabled};

use crate::auth::{ApiKeyAuth, AuthManager, OAuthFlow, Profile, Selection, classify_selection};
use crate::config::Config;

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Authenticate with Bitbucket (OAuth 2.0 or API key)
    Login {
        /// Use OAuth 2.0 authentication (interactive browser sign-in)
        #[arg(long, conflicts_with = "api_key")]
        oauth: bool,

        /// Use API key authentication (HTTP access token; for automation/CI)
        #[arg(long, conflicts_with = "oauth")]
        api_key: bool,

        /// Atlassian account email / Bitbucket username (for API key authentication)
        #[arg(long, env = "BITBUCKET_EMAIL", conflicts_with = "oauth")]
        email: Option<String>,

        /// API key (HTTP access token; for API key authentication, implies --api-key)
        #[arg(long, env = "BITBUCKET_API_TOKEN", conflicts_with = "oauth")]
        token: Option<String>,

        /// OAuth Client ID (for OAuth authentication)
        #[arg(long, env = "BITBUCKET_CLIENT_ID")]
        client_id: Option<String>,

        /// OAuth Client Secret (for OAuth authentication)
        #[arg(long, env = "BITBUCKET_CLIENT_SECRET")]
        client_secret: Option<String>,
    },

    /// Remove the credentials of the selected profile
    Logout,

    /// Show authentication status
    Status,

    /// List stored profiles (never prints secrets)
    List,
}

/// Row of the `auth list` table.
#[derive(Tabled)]
struct ProfileRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "METHOD")]
    method: String,
    #[tabled(rename = "USERNAME")]
    username: String,
    #[tabled(rename = "WORKSPACE")]
    workspace: String,
}

impl From<Profile> for ProfileRow {
    fn from(profile: Profile) -> Self {
        Self {
            name: profile.name,
            method: profile.method,
            username: profile.username.unwrap_or_default(),
            workspace: profile.default_workspace.unwrap_or_default(),
        }
    }
}

/// `auth login --workspace W` stores W as the logged-in profile's default workspace.
fn save_login_workspace(auth_manager: &AuthManager) -> Result<()> {
    match super::workspace_override() {
        Some(workspace) if !workspace.is_empty() => auth_manager.set_default_workspace(&workspace),
        _ => Ok(()),
    }
}

impl AuthCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            AuthCommands::Login {
                oauth,
                api_key,
                email,
                token,
                client_id,
                client_secret,
            } => {
                let auth_manager = AuthManager::new()?.for_login();

                let use_api_key = resolve_auth_method(
                    oauth,
                    api_key,
                    email.is_some() || token.is_some(),
                    client_id.is_some() || client_secret.is_some(),
                )?;

                if use_api_key {
                    ApiKeyAuth::authenticate(&auth_manager, email, token).await?;
                    return save_login_workspace(&auth_manager);
                }

                // OAuth 2.0 authentication.
                // Resolve consumer credentials from (in priority):
                // 1. CLI flags / env vars
                // 2. Previously stored credentials
                // 3. Interactive prompt (first-time only)
                let stored_consumer = auth_manager.get_credentials().ok().flatten().and_then(|c| {
                    c.oauth_consumer_credentials()
                        .map(|(id, secret)| (id.to_owned(), secret.to_owned()))
                });

                let client_id = client_id
                    .or_else(|| stored_consumer.as_ref().map(|(id, _)| id.clone()))
                    .or_else(|| {
                        println!();
                        println!("📋 OAuth Consumer Setup Required");
                        println!();
                        println!("To use OAuth authentication, create an OAuth consumer in Bitbucket:");
                        println!("1. Go to: https://bitbucket.org/[workspace]/workspace/settings/oauth-consumers/new");
                        println!("2. Set callback URL to ONE of these (pick any available port):");
                        println!("   • http://127.0.0.1:8080/callback");
                        println!("   • http://127.0.0.1:3000/callback");
                        println!("   • http://127.0.0.1:8888/callback");
                        println!("   • http://127.0.0.1:9000/callback");
                        println!("3. Select required permissions:");
                        println!("   ✓ Account (Read)");
                        println!("   ✓ Repositories (Read)");
                        println!("   ✓ Pull requests (Read, Write)");
                        println!("   ✓ Issues (Read, Write)");
                        println!("   ✓ Pipelines (Read, Write)");
                        println!("4. Copy the Key (Client ID) and Secret");
                        println!();

                        Input::<String>::new()
                            .with_prompt("OAuth Client ID (Key)")
                            .interact_text()
                            .ok()
                    })
                    .ok_or_else(|| anyhow::anyhow!("OAuth Client ID is required"))?;

                let client_secret = client_secret
                    .or_else(|| stored_consumer.map(|(_, secret)| secret))
                    .or_else(|| {
                        Input::<String>::new()
                            .with_prompt("OAuth Client Secret")
                            .interact_text()
                            .ok()
                    })
                    .ok_or_else(|| anyhow::anyhow!("OAuth Client Secret is required"))?;

                let oauth = OAuthFlow::new(client_id, client_secret);
                oauth.authenticate(&auth_manager).await?;

                save_login_workspace(&auth_manager)
            }

            AuthCommands::Logout => {
                let auth_manager = AuthManager::new()?;
                let (name, others_remain) = auth_manager.clear_credentials()?;

                if !others_remain {
                    let mut config = Config::load()?;
                    config.clear_auth();
                    config.save()?;
                }

                println!("{} Logged out of profile '{}'", "✓".green(), name);
                Ok(())
            }

            AuthCommands::List => {
                let profiles = AuthManager::new()?.profiles()?;

                if super::output_json() {
                    return super::print_json(&profiles);
                }

                if profiles.is_empty() {
                    println!(
                        "No profiles. Run {} to add one",
                        "bitbucket auth login".cyan()
                    );
                } else {
                    let rows: Vec<ProfileRow> = profiles.into_iter().map(Into::into).collect();
                    println!("{}", Table::new(rows));
                }
                Ok(())
            }

            AuthCommands::Status => {
                let auth_manager = AuthManager::new()?;
                let selection =
                    status_selection(&auth_manager.profiles()?, auth_manager.requested())?;
                if let StatusSelection::Ambiguous(report) = selection {
                    println!("{}", report);
                    return Ok(());
                }

                let config = Config::load()?;
                let credential = auth_manager.get_credentials()?;

                if let Some(credential) = credential {
                    println!("{} Authenticated", "✓".green());

                    let profile = auth_manager.selected_metadata()?;
                    if let Some(profile) = &profile {
                        println!("  {} {}", "Profile:".dimmed(), profile.name);
                    }
                    println!("  {} {}", "Method:".dimmed(), credential.type_name());

                    // Show username from credential for API keys, or the profile / config for OAuth
                    if let Some(username) = credential.username() {
                        println!("  {} {}", "Username:".dimmed(), username);
                    } else if let Some(username) = profile
                        .as_ref()
                        .and_then(|p| p.username.as_deref())
                        .or(config.username())
                    {
                        println!("  {} {}", "Username:".dimmed(), username);
                    }

                    if credential.needs_refresh() {
                        println!(
                            "  {} {}",
                            "Status:".dimmed(),
                            "Token needs refresh (will auto-refresh on next use)".yellow()
                        );
                    }

                    if let Some(workspace) = profile
                        .as_ref()
                        .and_then(|p| p.default_workspace.as_deref())
                        .or(config.default_workspace())
                    {
                        println!("  {} {}", "Workspace:".dimmed(), workspace);
                    }

                    match crate::api::BitbucketClient::from_auth_manager(&auth_manager).await {
                        Ok(client) => match client.get::<serde_json::Value>("/user").await {
                            Ok(user) => {
                                if let Some(display_name) = user.get("display_name") {
                                    println!(
                                        "  {} {}",
                                        "Display name:".dimmed(),
                                        display_name.as_str().unwrap_or("Unknown")
                                    );
                                }
                            }
                            Err(e) => {
                                println!("{} Credentials may be invalid: {}", "⚠".yellow(), e);
                            }
                        },
                        Err(e) => {
                            println!("{} Failed to create client: {}", "✗".red(), e);
                        }
                    }
                } else {
                    println!("{} Not authenticated", "✗".red());
                    println!();
                    println!("Run {} to authenticate", "bitbucket auth login".cyan());
                }

                Ok(())
            }
        }
    }
}

/// What `auth status` should report for the stored profiles.
#[derive(Debug, PartialEq, Eq)]
enum StatusSelection {
    NotAuthenticated,
    Selected(String),
    /// Several profiles and none chosen: the text to print instead of a status.
    Ambiguous(String),
}

/// Decide what `auth status` reports. An unknown requested name is an error;
/// with several profiles and no request, the report lists them (never a secret).
fn status_selection(profiles: &[Profile], requested: Option<&str>) -> Result<StatusSelection> {
    let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();

    Ok(match classify_selection(&names, requested)? {
        Selection::Empty => StatusSelection::NotAuthenticated,
        Selection::One(name) => StatusSelection::Selected(name),
        Selection::Ambiguous(_) => StatusSelection::Ambiguous(ambiguous_report(profiles)),
    })
}

fn ambiguous_report(profiles: &[Profile]) -> String {
    let mut report = String::from(
        "Several profiles are stored; choose one with --profile <name> or BITBUCKET_PROFILE.\n",
    );
    for profile in profiles {
        report.push_str(&format!(
            "\n  {}\n    Method: {}\n    Username: {}\n    Workspace: {}\n",
            profile.name,
            profile.method,
            profile.username.as_deref().unwrap_or("-"),
            profile.default_workspace.as_deref().unwrap_or("-"),
        ));
    }
    report
}

/// Resolve which authentication method to use.
///
/// Returns `true` for API key, `false` for OAuth 2.0.
///
/// Priority: explicit flag > method-implying inputs > interactive prompt.
fn resolve_auth_method(
    oauth: bool,
    api_key: bool,
    api_key_inputs_present: bool,
    oauth_inputs_present: bool,
) -> Result<bool> {
    if api_key || api_key_inputs_present {
        return Ok(true);
    }
    if oauth || oauth_inputs_present {
        return Ok(false);
    }

    println!();
    println!("{}", "Choose an authentication method".bold());
    println!();
    println!(
        "  {}  Browser-based sign-in. Recommended for interactive use.",
        "OAuth 2.0".cyan()
    );
    println!(
        "  {}     HTTP access token. For automation, CI, and headless environments.",
        "API key".cyan()
    );
    println!();

    let options = ["OAuth 2.0 (browser sign-in)", "API key (access token)"];

    let selection = Select::new()
        .with_prompt("Authentication method")
        .items(options)
        .default(0)
        .interact()
        .context("Failed to read authentication method selection")?;

    Ok(selection == 1)
}

#[cfg(test)]
mod tests {
    use super::{StatusSelection, resolve_auth_method, status_selection};
    use crate::auth::Profile;

    fn profile(name: &str, workspace: Option<&str>) -> Profile {
        Profile {
            name: name.to_string(),
            method: "API Key".to_string(),
            username: Some(format!("{name}-user")),
            default_workspace: workspace.map(String::from),
        }
    }

    // Proves: C9 several profiles and no choice report all of them with the hint
    #[test]
    fn profile_c9_several_profiles_without_choice_are_listed_with_a_hint() {
        let profiles = [profile("work", Some("acme")), profile("home", None)];

        let StatusSelection::Ambiguous(report) = status_selection(&profiles, None).unwrap() else {
            panic!("expected the ambiguous report");
        };

        for expected in [
            "work",
            "home",
            "API Key",
            "work-user",
            "home-user",
            "acme",
            "--profile",
            "BITBUCKET_PROFILE",
        ] {
            assert!(report.contains(expected), "{expected} missing in {report}");
        }
    }

    // Proves: C9 unknown requested name errors, naming it
    #[test]
    fn profile_c9_unknown_requested_profile_errors() {
        let profiles = [profile("work", None)];
        let message = status_selection(&profiles, Some("ghost"))
            .unwrap_err()
            .to_string();
        assert!(message.contains("ghost"), "{message}");
    }

    // Proves: C9 zero profiles is not authenticated, one is selected
    #[test]
    fn profile_c9_zero_profiles_is_unauthenticated_and_one_is_selected() {
        assert_eq!(
            status_selection(&[], None).unwrap(),
            StatusSelection::NotAuthenticated
        );
        assert_eq!(
            status_selection(&[profile("solo", None)], None).unwrap(),
            StatusSelection::Selected("solo".to_string())
        );
        assert_eq!(
            status_selection(&[profile("a", None), profile("b", None)], Some("b")).unwrap(),
            StatusSelection::Selected("b".to_string())
        );
    }

    #[test]
    fn explicit_api_key_flag_selects_api_key() {
        assert!(resolve_auth_method(false, true, false, false).unwrap());
    }

    #[test]
    fn explicit_oauth_flag_selects_oauth() {
        assert!(!resolve_auth_method(true, false, false, false).unwrap());
    }

    #[test]
    fn email_or_token_implies_api_key() {
        assert!(resolve_auth_method(false, false, true, false).unwrap());
    }

    #[test]
    fn oauth_inputs_imply_oauth() {
        assert!(!resolve_auth_method(false, false, false, true).unwrap());
    }

    #[test]
    fn api_key_inputs_win_over_oauth_inputs() {
        assert!(resolve_auth_method(false, false, true, true).unwrap());
    }
}
