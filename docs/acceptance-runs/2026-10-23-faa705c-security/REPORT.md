# Acceptance report: 2026-10-23-faa705c-security

Revision `faa705c5b757d19b5e0c0ccc4d0919e621d0a6ba` · profile security · windows

**Overall acceptance: 2 / 106 passed.** Cases that did not run, or have no test, stay in the denominator.

| System | Cases | Passed | Failed | Not implemented | Not run |
| --- | --- | --- | --- | --- | --- |
| agent-observability | 8 | 0 | 0 | 7 | 1 |
| browser-and-application-control | 8 | 0 | 0 | 8 | 0 |
| composable-harness-and-plugins | 8 | 0 | 0 | 8 | 0 |
| cost-and-performance | 8 | 0 | 0 | 8 | 0 |
| end-to-end | 10 | 0 | 0 | 10 | 0 |
| evaluation-and-self-improvement | 8 | 0 | 0 | 8 | 0 |
| integrated-verification-engine | 8 | 0 | 0 | 8 | 0 |
| local-and-remote-execution | 8 | 0 | 0 | 7 | 1 |
| model-agnostic-ai-engine | 8 | 0 | 0 | 4 | 4 |
| multi-agent-orchestration | 8 | 2 | 0 | 2 | 4 |
| persistent-project-memory | 8 | 0 | 0 | 8 | 0 |
| security-and-isolation | 8 | 0 | 0 | 8 | 0 |
| semantic-codebase-intelligence | 8 | 0 | 0 | 2 | 6 |

| ID | Case | Result | Capability (audit) |
| --- | --- | --- | --- |
| AI-01 | Adapter normalization | not_run | partial |
| AI-02 | Common task interface | not_run | implemented |
| AI-03 | Model switching with state | not_run | implemented |
| AI-04 | Rate limit handling | not_implemented | missing |
| AI-05 | Provider outage handling | not_run | implemented |
| AI-06 | Capability validation | not_implemented | missing |
| AI-07 | Budget exhaustion | not_implemented | missing |
| AI-08 | Local endpoint support | not_implemented | partial |
| MA-01 | Concurrent workers | not_run | implemented |
| MA-02 | Dependency ordering | not_run | implemented |
| MA-03 | Independent edits | not_run | implemented |
| MA-04 | Conflicting edits | not_run | partial |
| MA-05 | Scoped delegation | not_implemented | missing |
| MA-06 | Worker crash isolation | passed | partial |
| MA-07 | Parent cancellation | passed | implemented |
| MA-08 | Verified integration | not_implemented | partial |
| CI-01 | Repository indexing | not_run | implemented |
| CI-02 | Exact symbol lookup | not_run | implemented |
| CI-03 | Caller discovery | not_run | implemented |
| CI-04 | Scope-safe rename | not_implemented | missing |
| CI-05 | Incremental refresh | not_run | implemented |
| CI-06 | Invalid syntax handling | not_implemented | missing |
| CI-07 | Dependency impact | not_run | implemented |
| CI-08 | Symbol identity | not_run | implemented |
| PM-01 | Durable decisions | not_implemented | missing |
| PM-02 | Fresh-session retrieval | not_implemented | missing |
| PM-03 | Decision versioning | not_implemented | missing |
| PM-04 | Stale source detection | not_implemented | missing |
| PM-05 | Semantic retrieval | not_implemented | missing |
| PM-06 | Contradiction handling | not_implemented | missing |
| PM-07 | Untrusted memory content | not_implemented | missing |
| PM-08 | Project memory isolation | not_implemented | missing |
| VE-01 | Compiler diagnostics | not_implemented | missing |
| VE-02 | Failing test feedback | not_implemented | missing |
| VE-03 | Repair and rerun | not_implemented | missing |
| VE-04 | Lint enforcement | not_implemented | missing |
| VE-05 | Dependency advisory detection | not_implemented | missing |
| VE-06 | Structured edit integrity | not_implemented | missing |
| VE-07 | Bounded repair attempts | not_implemented | missing |
| VE-08 | Completion enforcement | not_implemented | missing |
| BA-01 | Local site launch | not_implemented | missing |
| BA-02 | Form submission | not_implemented | missing |
| BA-03 | Navigation chain | not_implemented | missing |
| BA-04 | Screenshot artifact | not_implemented | missing |
| BA-05 | Console error capture | not_implemented | missing |
| BA-06 | Network failure detection | not_implemented | missing |
| BA-07 | Mobile layout assertions | not_implemented | missing |
| BA-08 | Dynamic readiness | not_implemented | missing |
| PL-01 | Plugin registration | not_implemented | missing |
| PL-02 | Plugin disabling | not_implemented | missing |
| PL-03 | Command collisions | not_implemented | missing |
| PL-04 | Plugin fault isolation | not_implemented | missing |
| PL-05 | MCP interoperability | not_implemented | partial |
| PL-06 | Adapter substitution | not_implemented | partial |
| PL-07 | Plugin boundary enforcement | not_implemented | missing |
| PL-08 | API incompatibility | not_implemented | missing |
| OB-01 | Complete operation trace | not_implemented | partial |
| OB-02 | Invocation detail | not_implemented | partial |
| OB-03 | Usage fidelity | not_implemented | partial |
| OB-04 | Failure diagnostics | not_implemented | partial |
| OB-05 | Trace persistence | not_run | implemented |
| OB-06 | Resume correlation | not_implemented | partial |
| OB-07 | Secret redaction | not_implemented | missing |
| OB-08 | Deterministic replay | not_implemented | missing |
| CO-01 | Token cost arithmetic | not_implemented | missing |
| CO-02 | Cached token discounts | not_implemented | missing |
| CO-03 | Economy routing | not_implemented | partial |
| CO-04 | Quality routing | not_implemented | partial |
| CO-05 | Shared budget reservations | not_implemented | missing |
| CO-06 | Versioned price updates | not_implemented | missing |
| CO-07 | Large context warnings | not_implemented | missing |
| CO-08 | Usage attribution | not_implemented | partial |
| SE-01 | Forbidden file read | not_implemented | missing |
| SE-02 | Secret isolation | not_implemented | missing |
| SE-03 | Destructive action gating | not_implemented | partial |
| SE-04 | Network restrictions | not_implemented | missing |
| SE-05 | Cross-task isolation | not_implemented | partial |
| SE-06 | Repository injection resistance | not_implemented | missing |
| SE-07 | Scoped approval audit | not_implemented | partial |
| SE-08 | Alternate route bypass | not_implemented | missing |
| LR-01 | Local execution | not_run | implemented |
| LR-02 | Container equivalence | not_implemented | missing |
| LR-03 | Remote dispatch | not_implemented | missing |
| LR-04 | Desktop-independent jobs | not_implemented | missing |
| LR-05 | Connection recovery | not_implemented | partial |
| LR-06 | Remote worker death | not_implemented | missing |
| LR-07 | Remote cancellation | not_implemented | missing |
| LR-08 | Runtime compatibility | not_implemented | missing |
| EV-01 | Five-task smoke evaluation | not_implemented | missing |
| EV-02 | Cross-model reports | not_implemented | missing |
| EV-03 | Known failure detection | not_implemented | missing |
| EV-04 | Configuration comparison | not_implemented | missing |
| EV-05 | Reproducible evaluation setup | not_implemented | missing |
| EV-06 | Protected evaluation assets | not_implemented | missing |
| EV-07 | Dataset history | not_implemented | missing |
| EV-08 | Evaluated improvement proposals | not_implemented | missing |
| E2E-01 | User creates coding task > model selected > file changed > tests run | not_implemented | partial |
| E2E-02 | User starts three coding tasks in parallel | not_implemented | partial |
| E2E-03 | Browser agent develops and tests a simple UI | not_implemented | missing |
| E2E-04 | Provider fails halfway through execution | not_implemented | partial |
| E2E-05 | User cancels task during a tool operation | not_implemented | partial |
| E2E-06 | User closes desktop during remote execution | not_implemented | missing |
| E2E-07 | Plugin attempts forbidden operation | not_implemented | missing |
| E2E-08 | Agent completes task exceeding budget | not_implemented | missing |
| E2E-09 | User compares two models on identical task | not_implemented | partial |
| E2E-10 | Agent finishes a multi-file refactor | not_implemented | partial |

Evidence: `tests/reports/<runId>/<caseId>/<attempt>/` (result.json, manifest.json, artifacts, stdout.log, stderr.log).
