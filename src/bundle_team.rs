use std::collections::{BTreeMap, BTreeSet};

use age::secrecy::SecretString;
use anyhow::{Result, bail, ensure};

use crate::{bundle, scope::Scope, sync::Workspace};

pub struct PreparedBundle {
    pub passwords: BTreeMap<Scope, SecretString>,
    pub reset_from_team_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleAccess {
    ReadOnly,
    Mutating,
}

pub fn prepare_bundle_for_team(
    workspace: &Workspace,
    team_id: &str,
    access: BundleAccess,
) -> Result<PreparedBundle> {
    ensure!(
        workspace.bundle_path.exists(),
        "signing bundle {} does not exist",
        workspace.bundle_path.display()
    );

    let passwords = bundle::resolve_existing_passwords(&workspace.bundle_path, &Scope::ALL)?;
    ensure!(
        !passwords.is_empty(),
        "no signing bundle sections were unlocked"
    );

    let reset_from_team_ids = reset_bundle_if_team_changed(workspace, team_id, &passwords, access)?;
    Ok(PreparedBundle {
        passwords,
        reset_from_team_ids,
    })
}

fn reset_bundle_if_team_changed(
    workspace: &Workspace,
    team_id: &str,
    passwords: &BTreeMap<Scope, SecretString>,
    access: BundleAccess,
) -> Result<Vec<String>> {
    let existing_team_ids = collect_scope_team_ids(workspace, passwords)?;
    let reset_from_team_ids = existing_team_ids
        .into_values()
        .filter(|existing_team_id| existing_team_id != team_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    if reset_from_team_ids.is_empty() {
        return Ok(Vec::new());
    }

    if access == BundleAccess::ReadOnly {
        bail!(
            "signing bundle belongs to team(s) {}; read-only commands will not reset it to {}. Run a mutating ASC apply workflow to perform the cutover",
            reset_from_team_ids.join(", "),
            team_id
        );
    }

    ensure!(
        passwords.len() == Scope::ALL.len(),
        "signing bundle belongs to team(s) {}; resetting it to {} requires both developer and release bundle passwords",
        reset_from_team_ids.join(", "),
        team_id
    );

    bundle::initialize_bundle(&workspace.bundle_path, team_id, passwords)?;
    Ok(reset_from_team_ids)
}

fn collect_scope_team_ids(
    workspace: &Workspace,
    passwords: &BTreeMap<Scope, SecretString>,
) -> Result<BTreeMap<Scope, String>> {
    let mut team_ids = BTreeMap::new();
    for (&scope, password) in passwords {
        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        team_ids.insert(scope, state.team_id);
    }
    Ok(team_ids)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use age::secrecy::SecretString;
    use tempfile::tempdir;

    use super::{BundleAccess, reset_bundle_if_team_changed};
    use crate::{bundle, scope::Scope, sync::Workspace};

    fn passwords() -> BTreeMap<Scope, SecretString> {
        BTreeMap::from([
            (
                Scope::Developer,
                SecretString::from("developer-password".to_owned()),
            ),
            (
                Scope::Release,
                SecretString::from("release-password".to_owned()),
            ),
        ])
    }

    #[test]
    fn resets_bundle_to_new_team_id() {
        let tempdir = tempdir().unwrap();
        let workspace = Workspace::from_config_path(&tempdir.path().join("asc.json"));
        let passwords = passwords();
        bundle::initialize_bundle(&workspace.bundle_path, "OLDTEAM1234", &passwords).unwrap();

        let reset_from = reset_bundle_if_team_changed(
            &workspace,
            "NEWTEAM1234",
            &passwords,
            BundleAccess::Mutating,
        )
        .unwrap();

        assert_eq!(reset_from, vec!["OLDTEAM1234".to_owned()]);

        for scope in Scope::ALL {
            let mut runtime = workspace.create_runtime().unwrap();
            let restored = bundle::restore_scope(
                &mut runtime,
                &workspace.bundle_path,
                scope,
                &passwords[&scope],
            )
            .unwrap();
            assert_eq!(restored.team_id, "NEWTEAM1234");
            assert!(restored.bundle_ids.is_empty());
            assert!(restored.devices.is_empty());
            assert!(restored.certs.is_empty());
            assert!(restored.profiles.is_empty());
        }
    }

    #[test]
    fn read_only_access_refuses_to_reset_bundle_team() {
        let tempdir = tempdir().unwrap();
        let workspace = Workspace::from_config_path(&tempdir.path().join("asc.json"));
        let passwords = passwords();
        bundle::initialize_bundle(&workspace.bundle_path, "OLDTEAM1234", &passwords).unwrap();

        let error = reset_bundle_if_team_changed(
            &workspace,
            "NEWTEAM1234",
            &passwords,
            BundleAccess::ReadOnly,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("read-only commands will not reset it")
        );
    }
}
