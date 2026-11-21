# Focus reports

Run `mcbtodo timer report` with the local `mcbtodo-daemon` daemon running. Reports use the configured IANA timezone. Use `--timezone` to override it for one command.

```sh
mcbtodo timer report
mcbtodo timer report --date 2026-11-20 --json
mcbtodo timer report --week
mcbtodo timer report --week --date 2026-11-20
mcbtodo timer report --from 2026-11-01 --to 2026-11-30 \
  --project 10000000-0000-4000-8000-000000000001 --csv > focus.csv
```

The default report covers today. A week runs Monday through Sunday and contains the given `--date` or today. Both endpoints of `--from` and `--to` are inclusive. A range can span up to 366 days. The daemon assigns a session's entire elapsed time to its local start date, including time accrued after midnight or after a resume.

The selected Vault supplies current project membership and estimates. Use `--vault` to select a different Vault. The daemon's timer history belongs to the local workspace, and a project filter selects sessions whose WorkUnits currently belong to that project. The unfiltered report also includes unassigned sessions and UUIDs of WorkUnits absent from the selected Vault.

Each WorkUnit with recorded sessions in the period has one row. Actual time is the sum of its reported seconds across the period. Its estimate is the current `estimated_minutes` converted to seconds and counted once per row. Variance is actual seconds minus estimated seconds. Missing estimates produce no variance. Review counts identify sessions currently marked as needing review.

The daemon reports whole seconds per day and WorkUnit. The unfiltered summary sums the daemon's daily totals, and a project summary sums the selected WorkUnit rows. Rounding can make the unfiltered total differ from the sum of its rows. Each day's data is read separately, so the report can reflect timer changes during collection.

Markdown is the default. `--json` and `--csv` are mutually exclusive. Both use `date` and `end_date` for the first and last dates. CSV starts with a `total` row followed by `work_unit` rows in the same order as the Markdown and JSON reports. Empty CSV fields indicate absent values, including estimates and variances on the total row.
