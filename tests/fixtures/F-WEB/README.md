# F-WEB

A small deterministic site for the browser cases (BA-01 … BA-08), served by the test harness's
fixture server (`acceptance_tests::web_fixture`) on a free local port. Pages carry their identity in
`<body data-page="…">`; there are no animations or transitions, and nothing depends on the clock.

| Route | Page | Used by |
| --- | --- | --- |
| `/` | home: a banner in a known colour (`#1d6fb8`), a link into the tour | BA-01, BA-04 |
| `/tour/1`, `/tour/2`, `/tour/3` | three pages linked by "Next page" | BA-03 |
| `/form` | name and email; submits once to `POST /api/signup` and shows the stored record | BA-02 |
| `/error` | "Break it" throws `Error("fixture-boom-42")` | BA-05 |
| `/fail` | "Load data" calls `GET /api/fail`, which answers 500 with a marker | BA-06 |
| `/mobile` | a menu that collapses behind a "Menu" button under 600 px, and a form | BA-07 |
| `/modal` | "Open dialog" loads its content from `GET /api/slow` (800 ms); Confirm posts `POST /api/confirm` | BA-08 |

The server keeps what it receives (`GET /api/records`) and a log of every request.
