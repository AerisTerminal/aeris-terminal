# Known gaps and deferred work

Open gaps and deferred work. Each needs a maintainer decision or access before it can move; remove
an entry in the commit that closes it.

- **Maintainer-only checks:**
  - logoff flush (M8.3)
  - Rithmic login through the native vault (M6.9)
  - real Cognito sign-in
  - update and rollback on an installed app
  - macOS signing and notarization
  - code-signing certificate and key custody
  - Rithmic live qualification and conformance approval before production order routing
- **Lifecycle installer findings (M8.6 sandbox run):** fixed in `aeris_setup.iss` and checked on
  the maintainer's machine with staging generations 1 and 2. The launcher's `--install` used to
  fail after Inno had finished (an `ssPostInstall` exception is only displayed), so a silent install
  exited 0 without an active release. Moving it into the last file's `AfterInstall` was not enough:
  Inno Setup also discards exceptions raised there (its source catches them in
  `NotifyInstallEntry`), and a generation 1 setup over generation 2 still exited 0 after the
  launcher refused the downgrade, with generation 1's launcher left in place. A failed install now
  restores the replaced launcher, skips the shortcuts and launch entry and exits 20; that run left
  generation 2's launcher and shortcuts, and generation 2 still launched. A direct 1.x
  installation's `aeris_desktop.exe` is removed only after the install succeeds. The uninstall
  asked twice; the registered command is now `/SILENT /INTERACTIVE`, and an uninstall with it
  showed one prompt ("Remove my Aeris data" unchecked), removed the app, shortcuts and uninstall
  entry, and kept the workspace and trading store byte-identical. The shortcuts take the 1.x names:
  generation 1 over the public 1.2.0 install repointed both at the launcher and removed the root
  `aeris_desktop.exe`. Left: a failing install over a direct 1.x installation (not run), and Inno leaves a temporary uninstaller
  copy in `Aeris-Uninstall` after an uninstall.
- **Shutdown retirement:** a live window close has not been rerun since `retire_endpoint` stopped
  retiring an endpoint twice (`an_endpoint_retires_and_acknowledges_once`).
- **Out of scope by D3:** depth-aware partial fills, slippage, margin and buying power, currency
  conversion, and the Funding/MarginReserve ledger kinds (hooks only).
- **Old P&L data:** the `trade_pnl_cycles` and `risk_trade_cycles` tables and the per-fill P&L
  columns stay, unread, until a later removal migration.
- **Legacy migrations (LC-17, PL-16 migrations part):** the legacy data-root migration and the vault
  legacy-name probes stay until the maintainer confirms their machine is migrated.
- **Risk profile UI (TR-09 desktop part):** risk profile registration is not built, and no desktop
  code selects the futures `ChicagoClose` boundary.
- **Migrated instruments without a route:** these show "feed unavailable" until selected again;
  there is no automatic route resolution.
- **Exchange holiday data:** exchange rules report Open on holidays, and the staleness rule still
  protects trading.
- **Second launch:** it does not focus the existing window (that needs IPC).
- **Workspace-level account selection:** the per-pane selection is kept.
- **Broker relay edge cases:**
  - the reverse 30-second deadline has no deterministic test
  - an exclusive lane stays held after a timeout until the reconcile
  - a late cancel-all reply matches the next pending one
  - none of it has run against a live Rithmic gateway
- **Rithmic catalog:**
  - search and listing cache are tested only against a fake session
  - the listing keeps at most 128 products and is not refreshed in a session
  - an in-flight search cannot be cancelled
  - exchanges with spaces in their names cannot be exchange scopes
  - failed searches are not retried
- **Rithmic DBO and order levels:**
  - trade order IDs and implicit BBO sizes are not in the neutral contracts yet
  - JOP stays evidence only
  - begin/middle/end, clear and no-book frames were not observed live
  - the Queue column has not run against a live DBO stream
  - the 8-instrument budget is a default, not a measured limit
  - no queue position, iceberg or sweep detection is derived
  - raw-event persistence and book replay are unsupported
