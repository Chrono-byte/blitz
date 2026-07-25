# Helpdesk API

A small but production-shaped TypeScript/Effect application for Blitz with a React helpdesk UI. The API runs on the Blitz runtime, while Vite serves the TanStack Router frontend during development. HTTP, validation, domain orchestration, storage, and browser presentation stay in separate modules.

## Architecture

```text
src/
  config.ts              environment boundary
  domain.ts              domain types, validation, and typed errors
  ticket-repository.ts   replaceable repository backed by immutable Ref state
  ticket-service.ts      workflow rules exposed as an Effect service
  http.ts                Request/Response routing and error translation
  main.ts                Layer composition and the single Blitz runtime boundary
  web/                    React and TanStack Router frontend
test/
  service.test.ts        executable integration test with an isolated repository
```

Expected failures stay in the Effect error channel. The HTTP boundary translates them into stable JSON errors. Repository state is stored in an Effect `Ref`, and every update replaces its `Map` rather than mutating shared state. The same repository Layer is usable in production and tests.

## Install, check, and test

From this directory:

```sh
bun install
bun run typecheck
bun run check:api
bun run test:api
```

## Run

Start the Blitz API and Vite frontend in separate terminals:

```sh
bun run api
bun run dev
```

Open `http://localhost:5173`. Vite proxies API requests to the Blitz server at `127.0.0.1:8000`. Set `HOST` or `PORT` to change the API listener (and update `vite.config.ts` if needed).

Create and advance a ticket:

```sh
curl -s http://127.0.0.1:8000/health

curl -s -X POST http://127.0.0.1:8000/tickets \
  -H 'content-type: application/json' \
  -d '{"title":"Customer cannot reset password","description":"Reset email never arrives","priority":"high"}'

curl -s http://127.0.0.1:8000/tickets

curl -s -X PATCH http://127.0.0.1:8000/tickets/TKT-1/status \
  -H 'content-type: application/json' \
  -d '{"status":"in_progress"}'

curl -s -X PUT http://127.0.0.1:8000/tickets/TKT-1 \
  -H 'content-type: application/json' \
  -d '{"title":"Password reset is delayed","description":"The email arrived after 20 minutes","priority":"urgent"}'

curl -i -X DELETE http://127.0.0.1:8000/tickets/TKT-1
```

The bundled API currently uses in-memory state deliberately. Persistence can be added as another `TicketRepository` Layer without changing the service or HTTP modules.
