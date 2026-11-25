# Security

## Reporting a problem

Please report security problems privately through GitHub: on this repository's **Security** tab, choose **Report a vulnerability**. Don't open a public issue for them.

Useful things to include: what you found, how to reproduce it with the sample data, and which version you ran.

## What card-ledger is designed to do

- **The window** listens on `127.0.0.1` only. It refuses requests whose Host or Origin isn't its own, requires a custom header on every change, sets a strict Content-Security-Policy, and inserts all card text as plain text, never as HTML.
- **Your data** stays in your data folder (`%APPDATA%\card-ledger` by default). Nothing is uploaded.
- **The JustTCG key** is stored in Windows Credential Manager (elsewhere, a file in your data folder readable only by you). It's never printed, logged or written to the ledger database. It's sent only to `https://api.justtcg.com`, with redirects refused, and the window only ever sees its last four characters.
- **Outside requests** go only to TCGdex and, with a key, JustTCG, over HTTPS. A challenge or bot-check page stops the run rather than being worked around.

If you find behaviour that breaks any of these, that's a security bug; please report it as above.
