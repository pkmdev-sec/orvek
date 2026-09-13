use super::*;

#[tokio::test]
async fn rollout_home_supplies_global_instructions() -> Result<()> {
    let workspace = temporary_workspace("rollout-instructions-workspace")?;
    let rollout_home = temporary_workspace("rollout-instructions-home")?;
    std::fs::write(
        rollout_home.join("AGENTS.md"),
        "Use the rollout home instructions.",
    )?;

    run_global_instructions_case(
        &workspace,
        RolloutConfig::new(&rollout_home),
        None,
        "Use the rollout home instructions.",
        None,
    )
    .await?;

    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    Ok(())
}

#[tokio::test]
async fn explicit_codex_home_takes_precedence_over_rollout_home() -> Result<()> {
    let workspace = temporary_workspace("explicit-instructions-workspace")?;
    let rollout_home = temporary_workspace("explicit-instructions-rollout")?;
    let codex_home = temporary_workspace("explicit-instructions-home")?;
    std::fs::write(
        rollout_home.join("AGENTS.md"),
        "Do not use the rollout home instructions.",
    )?;
    std::fs::write(
        codex_home.join("AGENTS.md"),
        "Use the explicit Codex home instructions.",
    )?;

    run_global_instructions_case(
        &workspace,
        RolloutConfig::new(&rollout_home),
        Some(&codex_home),
        "Use the explicit Codex home instructions.",
        Some("Do not use the rollout home instructions."),
    )
    .await?;

    std::fs::remove_dir_all(workspace)?;
    std::fs::remove_dir_all(rollout_home)?;
    std::fs::remove_dir_all(codex_home)?;
    Ok(())
}

async fn run_global_instructions_case(
    workspace: &Path,
    rollout: RolloutConfig,
    codex_home: Option<&Path>,
    expected: &'static str,
    unexpected: Option<&'static str>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        let input = generation["input"].to_string();
        assert!(input.contains(expected), "{input}");
        if let Some(unexpected) = unexpected {
            assert!(!input.contains(unexpected), "{input}");
        }
        send_final(&mut socket, "resp-final").await
    });

    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let mut builder = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(workspace)
        .session_id(test_session_id());
    if let Some(codex_home) = codex_home {
        builder = builder.codex_home(codex_home);
    }
    let (agent, events) = builder.rollout(rollout).build()?;
    assert_eq!(
        agent
            .prompt("follow the applicable instructions")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    agent.flush_rollout().await?;
    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}
