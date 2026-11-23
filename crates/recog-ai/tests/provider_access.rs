//! `ProviderFactory::create` starts no provider below the `manual` level.

mod common;

use common::{Answer, KeyedFactory};
use recog_ai::AiError;
use recog_ai::access::Access;
use recog_ai::error::DISABLED;
use recog_ai::install::LegacyEnv;
use recog_ai::provider::{ProviderFactory, complete_in_order};
use recog_ai::settings::{AiSettingsStore, Fallback, ResolvedTask};

fn settings_at(access: Access) -> ResolvedTask {
    let dir = tempfile::tempdir().unwrap();
    let store =
        AiSettingsStore::new(dir.path(), Some(dir.path().join("prompts"))).with_legacy_env(LegacyEnv::default());
    let mut settings = store.resolve_task("title", None, Fallback::default()).unwrap();
    settings.providers = vec!["claude".into(), "codex".into()];
    settings.access = access;
    settings
}

fn both_installed() -> KeyedFactory {
    KeyedFactory::new(vec![
        ("claude", Answer::Text("from claude".into())),
        ("codex", Answer::Text("from codex".into())),
    ])
}

#[tokio::test]
async fn level_off_when_completing_in_order_then_disabled_and_nothing_built() {
    let factory = both_installed();
    let result = complete_in_order(&factory, &settings_at(Access::Off), "title", "prompt", &mut |_| {}).await;
    let Err(error) = result else { panic!("a provider answered at level off") };
    assert!(matches!(error, AiError::Disabled(_)), "{error:?}");
    assert_eq!(error.to_string(), DISABLED);
    assert_eq!(DISABLED, "AI-функции выключены в настройках recog.");
    assert!(factory.asked().is_empty());
}

#[test]
fn level_unasked_when_creating_then_disabled() {
    let factory = both_installed();
    let result = factory.create(&settings_at(Access::Unasked), "title", None);
    assert!(matches!(result, Err(AiError::Disabled(_))));
    assert!(factory.asked().is_empty());
}

#[test]
fn level_manual_when_creating_then_built() {
    let factory = both_installed();
    let provider = factory.create(&settings_at(Access::Manual), "title", None).unwrap();
    assert_eq!(provider.key(), "claude");
    assert_eq!(factory.asked(), ["claude"]);
}
