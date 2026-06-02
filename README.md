# Lookout

Simple time-tracking via periodic screenshots as a service, designed to be embedded into Hack Club programs. Lookout was previously known as "Collapse".

There are currently two official clients for this service:

- [Web React SDK](/clients/react/API.md) - for embedding the recorder in your web app.
- [Desktop App](https://github.com/hackclub/lookout/releases) - for download on Mac, Windows, and Linux.

> [!NOTE]
> If you're a YSWS program author hoping to integrate Lookout into your program, please reach out to me via Slack first. See [/docs/integration.md](/docs/integration.md) for technical information on how to integrate Lookout.
>
> If you're a Hack Clubber using Lookout and running into issues with Lookout, please reach out to the program's author (and not me). They'll forward the issue to me if needed. - @samliu

### Why does this exist?

Lookout is a [Lapse](https://lapse.hackclub.com) alternative with differing goals.

Lapse is a standalone, general purpose, time-lapse creation tool with [Hackatime](https://hackatime.hackclub.com) integration, to produce smooth time-lapse videos that is Hackatime compatible and can be shared.

Lookout is a service that processes screenshots for proof of time spent on a project. At it's core, Lookout accepts screenshots from clients (similar to Hackatime's heartbeats). Lookout needs to be integrated into other Hack Club programs to function.

## How it works... in a nutshell

Lookout is designed to be simple, resilient, and easy to integrate. Here's how it works at a high level:

1. A Hack Club program generates a session and shares it with the client.
2. The client begins capturing screenshots once per minute, uploading them as they are taken.
3. The server tracks the number and timing of screenshots received to validate time.
4. When the session finishes, Lookout stitches the screenshots into a time-lapse video.
5. The Hack Club program can retrieve the session results.

Sessions auto-pause after 10 minutes of inactivity and auto-stop after 24 hours of inactivity.

There is no concept of "users" or "accounts" in Lookout. Sessions are controlled by its token, which the Hack Club program saves and passes to the client. The Hack Club program decides when and how to use the data and results (i.e. push to Hackatime, use time directly, etc) with the same token. The concept of associating sessions with users or projects is up to the Hack Club program (A common approach is to store the session token to a user or project in the Hack Club program's database).
