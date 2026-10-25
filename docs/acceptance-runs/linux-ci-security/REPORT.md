# Acceptance report: ci-40153065374-1-security

Revision `cab824771bd3bcc9421a3dfd589af66ce2d1b135` · profile security · linux

**Overall acceptance: 16 / 106 passed.** Cases that did not run, or have no test, stay in the denominator.

| System | Cases | Passed | Failed | Blocked | Not implemented | Not run |
| --- | --- | --- | --- | --- | --- | --- |
| agent-observability | 8 | 0 | 0 | 0 | 0 | 8 |
| browser-and-application-control | 8 | 0 | 0 | 0 | 0 | 8 |
| composable-harness-and-plugins | 8 | 2 | 0 | 0 | 0 | 6 |
| cost-and-performance | 8 | 0 | 0 | 0 | 0 | 8 |
| end-to-end | 10 | 0 | 0 | 0 | 9 | 1 |
| evaluation-and-self-improvement | 8 | 1 | 0 | 0 | 0 | 7 |
| integrated-verification-engine | 8 | 0 | 0 | 0 | 0 | 8 |
| local-and-remote-execution | 8 | 3 | 0 | 0 | 0 | 5 |
| model-agnostic-ai-engine | 8 | 0 | 0 | 0 | 0 | 8 |
| multi-agent-orchestration | 8 | 2 | 0 | 0 | 0 | 6 |
| persistent-project-memory | 8 | 0 | 0 | 0 | 0 | 8 |
| security-and-isolation | 8 | 8 | 0 | 0 | 0 | 0 |
| semantic-codebase-intelligence | 8 | 0 | 0 | 0 | 0 | 8 |

| ID | Case | Result | Capability (audit) |
| --- | --- | --- | --- |
| AI-01 | Adapter normalization | not_run | partial |
| AI-02 | Common task interface | not_run | implemented |
| AI-03 | Model switching with state | not_run | implemented |
| AI-04 | Rate limit handling | not_run | missing |
| AI-05 | Provider outage handling | not_run | implemented |
| AI-06 | Capability validation | not_run | missing |
| AI-07 | Budget exhaustion | not_run | implemented |
| AI-08 | Local endpoint support | not_run | partial |
| MA-01 | Concurrent workers | not_run | implemented |
| MA-02 | Dependency ordering | not_run | implemented |
| MA-03 | Independent edits | not_run | implemented |
| MA-04 | Conflicting edits | not_run | partial |
| MA-05 | Scoped delegation | not_run | missing |
| MA-06 | Worker crash isolation | passed | implemented |
| MA-07 | Parent cancellation | passed | implemented |
| MA-08 | Verified integration | not_run | implemented |
| CI-01 | Repository indexing | not_run | implemented |
| CI-02 | Exact symbol lookup | not_run | implemented |
| CI-03 | Caller discovery | not_run | implemented |
| CI-04 | Scope-safe rename | not_run | missing |
| CI-05 | Incremental refresh | not_run | implemented |
| CI-06 | Invalid syntax handling | not_run | missing |
| CI-07 | Dependency impact | not_run | implemented |
| CI-08 | Symbol identity | not_run | implemented |
| PM-01 | Durable decisions | not_run | implemented |
| PM-02 | Fresh-session retrieval | not_run | implemented |
| PM-03 | Decision versioning | not_run | implemented |
| PM-04 | Stale source detection | not_run | implemented |
| PM-05 | Semantic retrieval | not_run | implemented |
| PM-06 | Contradiction handling | not_run | implemented |
| PM-07 | Untrusted memory content | not_run | implemented |
| PM-08 | Project memory isolation | not_run | implemented |
| VE-01 | Compiler diagnostics | not_run | implemented |
| VE-02 | Failing test feedback | not_run | implemented |
| VE-03 | Repair and rerun | not_run | implemented |
| VE-04 | Lint enforcement | not_run | implemented |
| VE-05 | Dependency advisory detection | not_run | implemented |
| VE-06 | Structured edit integrity | not_run | implemented |
| VE-07 | Bounded repair attempts | not_run | implemented |
| VE-08 | Completion enforcement | not_run | implemented |
| BA-01 | Local site launch | not_run | implemented |
| BA-02 | Form submission | not_run | implemented |
| BA-03 | Navigation chain | not_run | implemented |
| BA-04 | Screenshot artifact | not_run | implemented |
| BA-05 | Console error capture | not_run | implemented |
| BA-06 | Network failure detection | not_run | implemented |
| BA-07 | Mobile layout assertions | not_run | implemented |
| BA-08 | Dynamic readiness | not_run | implemented |
| PL-01 | Plugin registration | not_run | implemented |
| PL-02 | Plugin disabling | not_run | implemented |
| PL-03 | Command collisions | not_run | implemented |
| PL-04 | Plugin fault isolation | passed | implemented |
| PL-05 | MCP interoperability | not_run | implemented |
| PL-06 | Adapter substitution | not_run | implemented |
| PL-07 | Plugin boundary enforcement | passed | implemented |
| PL-08 | API incompatibility | not_run | implemented |
| OB-01 | Complete operation trace | not_run | implemented |
| OB-02 | Invocation detail | not_run | implemented |
| OB-03 | Usage fidelity | not_run | implemented |
| OB-04 | Failure diagnostics | not_run | implemented |
| OB-05 | Trace persistence | not_run | implemented |
| OB-06 | Resume correlation | not_run | implemented |
| OB-07 | Secret redaction | not_run | implemented |
| OB-08 | Deterministic replay | not_run | implemented |
| CO-01 | Token cost arithmetic | not_run | implemented |
| CO-02 | Cached token discounts | not_run | implemented |
| CO-03 | Economy routing | not_run | implemented |
| CO-04 | Quality routing | not_run | implemented |
| CO-05 | Shared budget reservations | not_run | implemented |
| CO-06 | Versioned price updates | not_run | implemented |
| CO-07 | Large context warnings | not_run | implemented |
| CO-08 | Usage attribution | not_run | implemented |
| SE-01 | Forbidden file read | passed | implemented |
| SE-02 | Secret isolation | passed | implemented |
| SE-03 | Destructive action gating | passed | implemented |
| SE-04 | Network restrictions | passed | implemented |
| SE-05 | Cross-task isolation | passed | implemented |
| SE-06 | Repository injection resistance | passed | implemented |
| SE-07 | Scoped approval audit | passed | implemented |
| SE-08 | Alternate route bypass | passed | implemented |
| LR-01 | Local execution | not_run | implemented |
| LR-02 | Container equivalence | passed | implemented |
| LR-03 | Remote dispatch | not_run | implemented |
| LR-04 | Desktop-independent jobs | not_run | implemented |
| LR-05 | Connection recovery | not_run | implemented |
| LR-06 | Remote worker death | passed | implemented |
| LR-07 | Remote cancellation | passed | implemented |
| LR-08 | Runtime compatibility | not_run | implemented |
| EV-01 | Five-task smoke evaluation | not_run | implemented |
| EV-02 | Cross-model reports | not_run | implemented |
| EV-03 | Known failure detection | not_run | implemented |
| EV-04 | Configuration comparison | not_run | implemented |
| EV-05 | Reproducible evaluation setup | not_run | implemented |
| EV-06 | Protected evaluation assets | passed | implemented |
| EV-07 | Dataset history | not_run | implemented |
| EV-08 | Evaluated improvement proposals | not_run | implemented |
| E2E-01 | User creates coding task > model selected > file changed > tests run | not_implemented | partial |
| E2E-02 | User starts three coding tasks in parallel | not_implemented | partial |
| E2E-03 | Browser agent develops and tests a simple UI | not_implemented | missing |
| E2E-04 | Provider fails halfway through execution | not_implemented | partial |
| E2E-05 | User cancels task during a tool operation | not_implemented | partial |
| E2E-06 | User closes desktop during remote execution | not_implemented | missing |
| E2E-07 | Plugin attempts forbidden operation | not_implemented | missing |
| E2E-08 | Agent completes task exceeding budget | not_run | implemented |
| E2E-09 | User compares two models on identical task | not_implemented | partial |
| E2E-10 | Agent finishes a multi-file refactor | not_implemented | partial |

Evidence: `tests/reports/<runId>/<caseId>/<attempt>/` (result.json, manifest.json, artifacts, stdout.log, stderr.log).
