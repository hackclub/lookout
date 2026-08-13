# Hosted recorder

The recorder Lookout serves itself, at `lookout.hackclub.com/session?token=…`
— the URL `POST /api/internal/sessions` has always returned as `sessionUrl`.

A program that doesn't want to build any recording UI redirects the user
here and is finished. The page reads the token from the URL, asks where the
frames should come from, and mounts the SDK's `<LookoutRecorder>` with the
answer.

```
?token=…  ──▶  chooser  ──┬──▶ desktop app   (lookout:// deep link)
                          ├──▶ this browser  (capture.mode "screen")
                          └──▶ camera        (capture.mode "camera")
```

## Scope

**Recording, and nothing else.** There is no gallery and no session browser.
A token is a capability that names one session; a page that turned it into a
browsable archive would be a different product with a different threat model.
Sessions still cannot be *created* here either — that needs an API key and a
program's backend.

The three terminal states the chooser never reaches (no token, a token the
server doesn't know, a session that already finished) say so in a sentence
instead of showing a recorder that can't work.

## Why the chooser lives here

Capture mode is provider-level config: `LookoutRecorder` reads
`capture.mode` and adapts its UI, it doesn't switch between modes. So the
choice has to be made *above* `LookoutProvider`, which is exactly what this
app is — a chooser and a provider around the SDK's own recorder. Everything
below that line (clips, pause/resume, the stop dialog, the cut editor) is
the SDK's, and this app inherits it without knowing about it.

## Layout

| Path | What it is |
|------|------------|
| `src/App.tsx` | Status read → chooser → recorder. The whole state machine. |
| `src/components/SourceChooser.tsx` | The one screen this app adds. |
| `src/components/DesktopHandoff.tsx` | `lookout://` handoff, with a fallback for "nothing happened". |
| `src/capabilities.ts` | What this browser can actually do, so dead options aren't offered. |
| `src/token.ts` | Token validation, URL params, and the deep-link URL shape. |

## Deployment

Built into the server image (see `Dockerfile.server`) and dropped at
`packages/server/public/session/`, where the SPA fallback in
`packages/server/src/index.ts` serves it for `/session*`. The origin root
stays the download landing page. The build's `base` is `/session/` — asset
URLs are absolute and carry that prefix, so it is not relocatable without a
rebuild.

## Development

```bash
npm run dev:hosted
```

Vite on `:5174` proxying `/api` to a local server on `:3001`. Open
`http://localhost:5174/session/?token=<token>` with a token from your local
server. Point at a deployed server instead with `VITE_API_BASE_URL`.
