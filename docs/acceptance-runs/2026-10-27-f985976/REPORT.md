# Acceptance report: 2026-10-27-f985976

Revision `f985976a69d51cca58583d04c056769129168701` · profile deterministic · windows

**Overall acceptance: 93 / 106 passed.** Cases that did not run, or have no test, stay in the denominator.

| System | Cases | Passed | Failed | Blocked | Not implemented | Not run |
| --- | --- | --- | --- | --- | --- | --- |
| agent-observability | 8 | 8 | 0 | 0 | 0 | 0 |
| browser-and-application-control | 8 | 8 | 0 | 0 | 0 | 0 |
| composable-harness-and-plugins | 8 | 7 | 0 | 0 | 0 | 1 |
| cost-and-performance | 8 | 8 | 0 | 0 | 0 | 0 |
| end-to-end | 10 | 9 | 0 | 1 | 0 | 0 |
| evaluation-and-self-improvement | 8 | 7 | 0 | 0 | 0 | 1 |
| integrated-verification-engine | 8 | 8 | 0 | 0 | 0 | 0 |
| local-and-remote-execution | 8 | 4 | 0 | 1 | 0 | 3 |
| model-agnostic-ai-engine | 8 | 8 | 0 | 0 | 0 | 0 |
| multi-agent-orchestration | 8 | 8 | 0 | 0 | 0 | 0 |
| persistent-project-memory | 8 | 8 | 0 | 0 | 0 | 0 |
| security-and-isolation | 8 | 2 | 0 | 0 | 0 | 6 |
| semantic-codebase-intelligence | 8 | 8 | 0 | 0 | 0 | 0 |

| ID | Case | Result | Capability (audit) |
| --- | --- | --- | --- |
| AI-01 | Adapter normalization | passed | implemented |
| AI-02 | Common task interface | passed | implemented |
| AI-03 | Model switching with state | passed | implemented |
| AI-04 | Rate limit handling | passed | implemented |
| AI-05 | Provider outage handling | passed | implemented |
| AI-06 | Capability validation | passed | implemented |
| AI-07 | Budget exhaustion | passed | implemented |
| AI-08 | Local endpoint support | passed | implemented |
| MA-01 | Concurrent workers | passed | implemented |
| MA-02 | Dependency ordering | passed | implemented |
| MA-03 | Independent edits | passed | implemented |
| MA-04 | Conflicting edits | passed | partial |
| MA-05 | Scoped delegation | passed | implemented |
| MA-06 | Worker crash isolation | passed | implemented |
| MA-07 | Parent cancellation | passed | implemented |
| MA-08 | Verified integration | passed | implemented |
| CI-01 | Repository indexing | passed | implemented |
| CI-02 | Exact symbol lookup | passed | implemented |
| CI-03 | Caller discovery | passed | implemented |
| CI-04 | Scope-safe rename | passed | implemented |
| CI-05 | Incremental refresh | passed | implemented |
| CI-06 | Invalid syntax handling | passed | implemented |
| CI-07 | Dependency impact | passed | implemented |
| CI-08 | Symbol identity | passed | implemented |
| PM-01 | Durable decisions | passed | implemented |
| PM-02 | Fresh-session retrieval | passed | implemented |
| PM-03 | Decision versioning | passed | implemented |
| PM-04 | Stale source detection | passed | implemented |
| PM-05 | Semantic retrieval | passed | implemented |
| PM-06 | Contradiction handling | passed | implemented |
| PM-07 | Untrusted memory content | passed | implemented |
| PM-08 | Project memory isolation | passed | implemented |
| VE-01 | Compiler diagnostics | passed | implemented |
| VE-02 | Failing test feedback | passed | implemented |
| VE-03 | Repair and rerun | passed | implemented |
| VE-04 | Lint enforcement | passed | implemented |
| VE-05 | Dependency advisory detection | passed | implemented |
| VE-06 | Structured edit integrity | passed | implemented |
| VE-07 | Bounded repair attempts | passed | implemented |
| VE-08 | Completion enforcement | passed | implemented |
| BA-01 | Local site launch | passed | implemented |
| BA-02 | Form submission | passed | implemented |
| BA-03 | Navigation chain | passed | implemented |
| BA-04 | Screenshot artifact | passed | implemented |
| BA-05 | Console error capture | passed | implemented |
| BA-06 | Network failure detection | passed | implemented |
| BA-07 | Mobile layout assertions | passed | implemented |
| BA-08 | Dynamic readiness | passed | implemented |
| PL-01 | Plugin registration | passed | implemented |
| PL-02 | Plugin disabling | passed | implemented |
| PL-03 | Command collisions | passed | implemented |
| PL-04 | Plugin fault isolation | passed | implemented |
| PL-05 | MCP interoperability | passed | implemented |
| PL-06 | Adapter substitution | passed | implemented |
| PL-07 | Plugin boundary enforcement | not_run | implemented |
| PL-08 | API incompatibility | passed | implemented |
| OB-01 | Complete operation trace | passed | implemented |
| OB-02 | Invocation detail | passed | implemented |
| OB-03 | Usage fidelity | passed | implemented |
| OB-04 | Failure diagnostics | passed | implemented |
| OB-05 | Trace persistence | passed | implemented |
| OB-06 | Resume correlation | passed | implemented |
| OB-07 | Secret redaction | passed | implemented |
| OB-08 | Deterministic replay | passed | implemented |
| CO-01 | Token cost arithmetic | passed | implemented |
| CO-02 | Cached token discounts | passed | implemented |
| CO-03 | Economy routing | passed | implemented |
| CO-04 | Quality routing | passed | implemented |
| CO-05 | Shared budget reservations | passed | implemented |
| CO-06 | Versioned price updates | passed | implemented |
| CO-07 | Large context warnings | passed | implemented |
| CO-08 | Usage attribution | passed | implemented |
| SE-01 | Forbidden file read | not_run | implemented |
| SE-02 | Secret isolation | not_run | implemented |
| SE-03 | Destructive action gating | not_run | implemented |
| SE-04 | Network restrictions | not_run | implemented |
| SE-05 | Cross-task isolation | not_run | implemented |
| SE-06 | Repository injection resistance | passed | implemented |
| SE-07 | Scoped approval audit | passed | implemented |
| SE-08 | Alternate route bypass | not_run | implemented |
| LR-01 | Local execution | passed | implemented |
| LR-02 | Container equivalence | blocked | implemented |
| LR-03 | Remote dispatch | not_run | implemented |
| LR-04 | Desktop-independent jobs | passed | implemented |
| LR-05 | Connection recovery | passed | implemented |
| LR-06 | Remote worker death | not_run | implemented |
| LR-07 | Remote cancellation | not_run | implemented |
| LR-08 | Runtime compatibility | passed | implemented |
| EV-01 | Five-task smoke evaluation | passed | implemented |
| EV-02 | Cross-model reports | passed | implemented |
| EV-03 | Known failure detection | passed | implemented |
| EV-04 | Configuration comparison | passed | implemented |
| EV-05 | Reproducible evaluation setup | passed | implemented |
| EV-06 | Protected evaluation assets | not_run | implemented |
| EV-07 | Dataset history | passed | implemented |
| EV-08 | Evaluated improvement proposals | passed | implemented |
| E2E-01 | User creates coding task > model selected > file changed > tests run | passed | implemented |
| E2E-02 | User starts three coding tasks in parallel | passed | implemented |
| E2E-03 | Browser agent develops and tests a simple UI | passed | implemented |
| E2E-04 | Provider fails halfway through execution | passed | implemented |
| E2E-05 | User cancels task during a tool operation | passed | implemented |
| E2E-06 | User closes desktop during remote execution | passed | implemented |
| E2E-07 | Plugin attempts forbidden operation | blocked | implemented |
| E2E-08 | Agent completes task exceeding budget | passed | implemented |
| E2E-09 | User compares two models on identical task | passed | implemented |
| E2E-10 | Agent finishes a multi-file refactor | passed | implemented |

Evidence: `tests/reports/<runId>/<caseId>/<attempt>/` (result.json, manifest.json, artifacts, stdout.log, stderr.log).
