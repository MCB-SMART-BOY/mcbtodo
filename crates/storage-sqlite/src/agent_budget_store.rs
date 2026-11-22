use std::time::Duration;

use super::*;

const MAX_ATTEMPTS: u32 = 4;
const MAX_WALL_MS: u32 = 60_000;

pub(super) fn install(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_agent_budgets (
             run_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_runs(id),
             attempts INTEGER NOT NULL CHECK (attempts >= 0),
             wall_ms INTEGER NOT NULL CHECK (wall_ms >= 0)
         ) STRICT;
         CREATE TRIGGER IF NOT EXISTS workflow_agent_budget_init
         AFTER INSERT ON workflow_runs BEGIN
             INSERT INTO workflow_agent_budgets(run_id, attempts, wall_ms)
             VALUES(NEW.id, 0, 0);
         END;",
    )?;
    Ok(())
}

impl RunStore {
    /// Reserve the remaining wall time and claim the Worker attempt in one
    /// writer transaction. The budget is not charged if the claim rolls back.
    pub(super) fn reserve_and_claim_agent(
        &self,
        claim: ClaimAttempt<'_>,
        has_prior_agent_step: bool,
    ) -> Result<AgentAttemptClaim> {
        let tx = self.write_transaction()?;
        let run = self
            .get(claim.run_id)?
            .context("workflow run was not found")?;
        ensure!(
            run.state == RunState::Pending,
            "Agent claim requires a pending run"
        );
        let id = run.id.to_string();
        let usage: Option<(u32, u32)> = tx
            .query_row(
                "SELECT attempts, wall_ms FROM workflow_agent_budgets WHERE run_id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (attempts, wall_ms) = match usage {
            Some(usage) => usage,
            // Earlier databases have no account. Start one only if the prior
            // trace contains no executed Agent whose elapsed time is unknown.
            None if !has_prior_agent_step => (0, 0),
            None => return Err(AgentBudgetDenied::Untracked.into()),
        };
        if attempts >= MAX_ATTEMPTS {
            return Err(AgentBudgetDenied::Calls {
                limit: MAX_ATTEMPTS,
            }
            .into());
        }
        if wall_ms >= MAX_WALL_MS {
            return Err(AgentBudgetDenied::WallTime.into());
        }
        tx.execute(
            "INSERT INTO workflow_agent_budgets(run_id, attempts, wall_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(run_id) DO UPDATE
             SET attempts=excluded.attempts, wall_ms=excluded.wall_ms",
            params![id, attempts + 1, MAX_WALL_MS],
        )?;
        let attempt = Self::insert_attempt(&tx, run, claim)?;
        tx.commit()?;
        Ok(AgentAttemptClaim {
            attempt,
            execution_window: Duration::from_millis(u64::from(MAX_WALL_MS - wall_ms)),
        })
    }
}

pub(super) fn settle(tx: &Transaction<'_>, run: &RunSummary, unused: Duration) -> Result<()> {
    // Floor the refund so fractional milliseconds are charged to the attempt.
    let unused_ms = u32::try_from(unused.as_millis())?;
    if unused_ms == 0 {
        return Ok(());
    }
    ensure!(
        run.state == RunState::Running,
        "Agent settlement requires a running claim"
    );
    let changed = tx.execute(
        "UPDATE workflow_agent_budgets SET wall_ms=wall_ms-?2
         WHERE run_id=?1 AND wall_ms=?3 AND attempts>0 AND wall_ms>=?2",
        params![run.id.to_string(), unused_ms, MAX_WALL_MS],
    )?;
    ensure!(changed == 1, "Agent wall-time reservation is missing");
    Ok(())
}
