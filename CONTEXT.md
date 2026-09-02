# Context: Portside — Localhost Dashboard

A menu bar app showing which local servers are running on this machine, which
project each belongs to, and which are safe to stop.

The language here is the language the user reads on screen. Where a plain word and a
technical word compete, the plain word wins — the audience is developers who have
lost track of their own servers, not people debugging networking.

## Glossary

### Server
A running program holding a local address the user could open in a browser. The unit
of display and the unit of stopping — stopping acts on the program, never on the
address alone.

One Server may hold **several ports** (commonly by answering on two addresses at
once, or serving a live-reload channel alongside the page). It stays one Server, one
row, because it is one thing the user would stop.

Called a Server rather than a process or a listener: the user recognises the thing
they started with `dev`, not the operating system's idea of it.

### Port
The number the user types after `localhost:`. An attribute of a Server, never an
identity for one.

Two Servers can hold the same number at the same time without conflict, by answering
on different addresses — observed: two different projects both on 4399, both
working. So the dashboard never assumes a number identifies one thing.

### Project
The repository a Server was started from, found by walking up from the Server's
working directory to the nearest sign of a project root.

Never a fixed location — the tool runs on any machine, for any user, and no path is
assumed.

One Project may own **several Servers** at once. Making that visible is the point of
grouping by Project: a project quietly holding two forgotten Servers is the case the
user cannot currently see.

### Package
The part of a Project a Server belongs to, when the Project holds several — the
difference between "vala-platform" and the specific package inside it that is
running.

A Server is shown as Project plus Package, because in a project holding several
packages the Project alone does not say which one is running.

### Guessed Project
A Project name the tool is unsure about, because the Server's working directory
does not reflect what it is actually serving. Happens with background services that
hold ports for other things: the directory records where the service was started
long ago, which may be an unrelated project.

Observed: a container service claiming a project it has nothing to do with, while
holding database ports.

Shown as uncertain, never as fact, and never used to decide something is safe to
stop.

### Belongs To
The application a Server is part of, when the Server is a background piece of a
larger app the user knows by name — an editor, a git client, a container tool.

This is the name used whenever the user is asked to confirm stopping it, because it
is the name they recognise. The user knows they are quitting their editor; they do
not recognise the name of the helper process inside it.

### Forgotten
A Server the user no longer wants running.

This is **the user's judgement**, which the tool supports with evidence rather than
making on their behalf. No single observation establishes it, and the two obvious
ones were both tested and failed:

- **Old does not mean forgotten.** A service running seven days was deliberate.
- **Unused does not mean forgotten.** Every Server observed had nobody connected,
  including wanted ones — a closed browser tab looks exactly like an abandoned
  server.

So the tool never labels a Server forgotten. It shows what it knows and lets the
user decide.

### Unattended
A Server whose parent program is gone — the terminal or coding session that started
it has exited, and the Server outlived it.

Observed on every development server started by a coding agent, which is the source
of the user's problem: the agent finishes, the Server stays. Unattended means nobody
is watching this Server, which is a hint toward Forgotten but not proof.

It also means the tool cannot tell which session started a Server. That link is gone
once the parent exits, and no amount of inspection recovers it.

### Responding
Whether a Server still answers when something connects to it.

A Server can hold its address while serving nothing — observed: a development server
holding its port for three days, refusing every connection, identical in age and
usage to a healthy one beside it.

This is the only check that reliably found a genuinely dead Server, so every Server
is checked continuously. The check is a bare connection carrying no request, which
is what makes it safe to run against databases and mail services that would
otherwise log errors.

### Title
What a Server is serving, in the user's terms — the title of the page it returns,
rather than the command that launched it.

Learning it means making a real request to the user's own server, so it is done only
for the user's own development Servers, and remembered rather than repeated: what a
Server is serving does not change while it keeps running. Renewed when a Server
stops Responding, or when asked.

### Not Yours
A port that is held, but by someone the tool cannot see — another user of the same
machine.

Without special permission the tool sees only the current user's Servers, so a port
can be genuinely occupied and appear as nothing at all. It is reported honestly as
in use by someone else. It is never quietly left out, and never shown as free, since
either would send the user hunting for a conflict the tool could see but did not
mention.

### Reachable From
Whether a Server answers only this machine, or every machine on the network.

Observed: database ports open to the whole network, reachable by anyone nearby.
Shown because the tool already knows it, and because the user has no other easy way
to notice.

## Stopping

### Kind
What a Server is, which determines what stopping it destroys, and therefore how much
the tool gets out of the way. Every Server is exactly one Kind.

- **Development server** — belongs to a Project and serves only that Project. The
  only Kind the user is expected to stop routinely.
- **Part of an app** — stopping it quits the whole application it belongs to,
  including unsaved work.
- **Background service** — holds ports on behalf of other things, so stopping it
  destroys those things rather than just the one address. Carries a Guessed Project.
- **Your own tool** — a program the user runs on purpose that belongs to no Project:
  a personal agent, a worker, a daemon they wrote or installed themselves. Shown in
  full, never stopped through this tool.
- **Part of macOS** — belongs to the system and is not stopped through this tool at
  all; the user is told where it is actually turned off.

A Server with no Project is never treated as a development server. Having no Project
is exactly the case where the tool cannot know What This Stops, so it is shown and
left alone rather than guessed at.

### What This Stops
Everything lost by stopping a Server, beyond the Server itself. Always stated before
stopping, in the user's own words — the name of the application they would lose, or
what a background service is holding up — never as a process name or a port number.

Every stop is confirmed. What makes a dangerous stop feel different from a routine
one is **what the confirmation names**, not how many clicks it takes.

### Watch Only
Servers the tool shows but never offers to stop, whatever the user clicks.

Covers what belongs to macOS, and what belongs to the user but to no Project — a
personal agent or worker running deliberately. The user wants to *see* these,
confirm they are up, and be left alone about them.

Being Watch Only is not a judgement that a Server matters more. It is an admission
that the tool cannot know what stopping it would cost, and that a dashboard the user
opens to tidy development servers is the wrong place to end a program holding up
their working day.

### Stop Everything
Stopping many Servers at once.

Limited to development servers. Nothing else is ever included, because one
confirmation cannot honestly describe several different consequences at the same
time. Everything else is stopped one at a time, or not at all.

### Stopping
Asking a Server to stop, and checking that it did.

Always asked politely first, giving the Server a chance to finish what it is doing.
A Server may refuse: stopping is a request, not a guarantee.

Aimed at the Server and everything it started, since a surviving child can keep the
address held after its parent is gone.

### Stopped
The condition that means stopping worked: the address is free again.

Not the same as having asked. A Server can accept the request, exit, and leave its
address held by something it started. So the tool checks afterwards, and a Server
that did not let go is shown as still running rather than quietly disappearing.

### Force Stop
Ending a Server without letting it finish, after politely asking has failed.

Never automatic, and never something the tool escalates to on its own — it throws
away whatever the Server had not yet saved. Offered only after a polite stop is seen
to fail, and confirmed on its own, because the user agreed to a polite stop and not
to this.

## Memory

### Keep Running
A mark the user puts on a Server saying they know about it and want it left alone.

The one thing the tool remembers between runs. Without it the user would re-judge
the same long-running service every single time they open the dashboard.

Remembered by what the Server is — its project and command — rather than by any
number the system assigns it, since those change every time it restarts.

This is the honest half of Forgotten: the tool never decides what the user has
forgotten, but it can be told what they have not, and stop drawing attention there.
Anything unmarked stays visible.

### Right Now
The tool shows the present moment only. It keeps no history of what ran yesterday,
because how long a Server has been up — which it can see directly — already answers
the question the user is actually asking.
