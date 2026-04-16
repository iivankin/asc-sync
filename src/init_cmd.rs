use std::io::{self, IsTerminal, Write};

use anyhow::{Result, bail, ensure};

use crate::{auth_store, cli::InitArgs, config_io, schema};

pub fn run(args: &InitArgs) -> Result<()> {
    ensure!(
        !args.config.exists(),
        "config {} already exists",
        args.config.display()
    );

    let team_id = match &args.team_id {
        Some(team_id) => team_id.trim().to_owned(),
        None => select_team_id_from_auth_store()?,
    };
    ensure!(!team_id.is_empty(), "team ID cannot be empty");

    let template = schema::init_config_template(&team_id);
    config_io::write_pretty_json(&args.config, &template)?;
    println!("Created {}", args.config.display());
    Ok(())
}

fn select_team_id_from_auth_store() -> Result<String> {
    let team_ids = auth_store::stored_team_ids()?;
    match team_ids.len() {
        0 => bail!(
            "no imported App Store Connect auth entries found; run `asc-sync auth import` first or pass --team-id explicitly"
        ),
        1 => Ok(team_ids[0].clone()),
        _ => prompt_team_selection(&team_ids),
    }
}

fn prompt_team_selection(team_ids: &[String]) -> Result<String> {
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "`init` requires an interactive terminal when multiple imported team IDs exist; pass --team-id explicitly"
    );

    println!("Select teamId for asc.json:");
    for (index, team_id) in team_ids.iter().enumerate() {
        println!("  {}. {}", index + 1, team_id);
    }

    let mut stdout = io::stdout().lock();
    loop {
        write!(stdout, "Choice [1-{}]: ", team_ids.len())?;
        stdout.flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();
        let Ok(choice) = trimmed.parse::<usize>() else {
            eprintln!("Enter a number from 1 to {}.", team_ids.len());
            continue;
        };
        if (1..=team_ids.len()).contains(&choice) {
            return Ok(team_ids[choice - 1].clone());
        }
        eprintln!("Enter a number from 1 to {}.", team_ids.len());
    }
}
