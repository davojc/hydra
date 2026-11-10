# What we build, and where

**Keep this file true.** It's the planning session's picture of the product and its code.
Every fact says where it came from (as of 2026-11-04), so it can be checked. When it's wrong
or missing something, fix it in a pull request to this repo: every planning session after that
uses the new version. When a project depends on something not described here, read the repo
or ask a question rather than guess.

## The product

**Interu**, by iov42: supply-chain traceability and compliance, mostly for companies under
the **EU Deforestation Regulation (EUDR)**: producers, traders and brands, supported by our
Customer Success team (CS).

- Companies record what they make, buy and move:
  - **items**, **products** (with **variants**) and **materials**;
  - **locations**, with GPS and GeoJSON plots;
  - **processes**, which turn inputs into outputs;
  - **deliveries** and **purchase orders**;
  - **documents** and **certificates** attached to any of them.

  Together these build a **supply chain** that Interu shows as a graph.
- **Getting data in.** Records are added by hand, by **bulk add**, or with the **CSV import
  wizard**: upload, validate, review in a grid, then propose and confirm. A re-import
  updates rows matched on their **alt_id**. CSV export lets people download, edit and upload
  again.
  - Imports ship one entity at a time, called **slices**.
  - The next lane after CSV is AI document ingestion.
  - (data-onboarding STATUS.md, 2026-09-11)
- **Sharing.** Organisations connect as **partners** (**connections**).
  - A company **sends** records to a partner, who sees them in their **inbox**.
  - The partner accepts and **maps** each record: link it to one they already have, or add
    it as new ("transfer or map").
  - **Link sharing** gives a read-only page to someone without an account.
- **EUDR reports.** Interu prepares **due diligence statements (DDS)** for items, products,
  deliveries and purchase orders, with risk assessments, from the traced data.
- **Organisation setup** (Settings): users and invitations, custom fields, tags, document and
  location types, units, standards, rulesets, risk templates, imports and notifications.
- An internal **admin console** shows sign-ups, activity, translation use and feature flags.

Words to use with customers and CS: record, item, product, location, delivery, share,
partner, inbox, map, import, statement (DDS). Avoid code words: entity, aggregate, DTO,
GraphQL.

## The repos

| Repo | What's there | Source |
| --- | --- | --- |
| `iov42/version2-frontend` | The Interu web app (below). Also its end-to-end tests, in `e2e/`. | package.json, CLAUDE.md |
| `iov42/version-2-backend` | The Interu backend and API (below). | build files, README |
| `iov42/api-designs` | API contracts, written in TypeSpec and compiled to OpenAPI, under `design/specs/`. These cover identity, directory, messaging, permissions, entitlements and admin. Those services' API changes are agreed here **before** they're built. The Interu backend's own API is defined in its code instead. | CLAUDE.md |
| `iov42/identity-platform` | Sign-in (an OAuth server, with Clerk), organisations, users, connections between organisations, roles and entitlements. .NET and Postgres. Its endpoints start in api-designs. | README, CLAUDE.md |
| `iov42/solutions-notification-v2` | The notification and email service. Java, Spring Boot. Shared with the older Timberchain product, so a change must not break either. | README, build.gradle |
| `iov42/svc-usage` | Usage metering and limit warnings per organisation. It observes and notifies, and never blocks. .NET 10. Has its own API rules (ADR-0047). | README, CLAUDE.md |
| `iov42/interu-admin-console` | The internal admin console: Next.js and React. It only reads: new data for it needs a report endpoint in identity-platform or the backend first. | README |
| `iov42/interu-playwright` | The older end-to-end suite (Playwright BDD). Don't add to it: new scenarios go in `version2-frontend/e2e`. | Jordan, 2026-11-04 |
| `iov42/cypress-bdd-pom` | The Cypress suite a backend pull request's `/re-run` runs against its branch deployment. | backend trigger-run-qa-tests.yaml |
| `iov42/interu-localisation` | Translations. The frontend's English strings are pushed here by CI and translated here. | frontend .github/workflows/update-locales.yaml |

### The web app: `iov42/version2-frontend`

- **Stack:**
  - Angular 21 (standalone components and signals), Angular Material and AG Grid Enterprise.
  - Its own design-system primitives, in `src/app/shared/components/design-system`.
  - No store library.
- **Where features live:** `src/app/components/<area>/`. The routes are in `src/app/app-routes.ts`.
  - Records: items, products, materials, locations, processes, deliveries, purchase-orders,
    certificates, documents, supply-chains.
  - Sharing: inbox (with transfer-or-map-data), shared-link.
  - Everything else: reports (eudr, delivery-report), dashboard, organisations, invitations,
    settings (the organisation setup), user-settings, admin (system admin), guest (sign-in).
  - Shared building blocks are in `components/shared/` (stepper, bulk-add, data-imports,
    overlay) and `src/app/shared/components/` (dialogs, side panels, map, document viewer).
- **Talking to the backend:**
  - Hand-written services, `src/app/shared/services/api/<entity>-api.service.ts`.
  - REST through `ApiService`, and GraphQL through `GraphApiService`.
  - Queries are in `src/app/shared/queries/`.
  - Tables page on the server.
- **Feature flags:**
  - Run on GrowthBook.
  - Keys are in `src/app/shared/enums/feature-flag.enum.ts`, checked with `FeatureFlagService`.
  - Routes are gated by `FeatureFlagGuard`.
  - A new flag also needs creating in GrowthBook.
- **Copy:**
  - Every string is translatable, with `$localize` / `i18n="@@id"`.
  - Only the English source, `src/locale/messages.xlf`, is edited. Translation happens in
    interu-localisation.
  - Strings that exist already come from `TextConstants`.
- **Tests:**
  - Unit tests are Vitest specs next to the code.
  - End-to-end tests are Playwright with Gherkin features in `e2e/features/`, steps in
    `e2e/features/steps/` and page objects in `e2e/pages/`. **Every new end-to-end scenario
    goes here** (Jordan, 2026-11-04), as a ticket in this repo. `@smoke` marks the ones run on
    pull requests.
- **Conventions:**
  - The repo's `CLAUDE.md` and `.claude/skills/` (architecture, data-layer, design-system,
    ag-grid, i18n, unit-testing) are the rules for the work itself.
  - Branches are named after the ticket key.

### The backend: `iov42/version-2-backend`

- **Stack:**
  - Java 21 and Spring Boot 4, on PostgreSQL. (build.gradle)
  - A modular monolith that keeps reads and writes apart:
    - `api` holds the REST controllers;
    - `command-service` makes the changes and owns the data;
    - `query-service` reads;
    - `graphql-support` serves the GraphQL the web app reads with;
    - `background-worker` runs jobs: imports and EUDR;
    - `extensions` holds the EUDR DDS, geospatial and AI-translation code.

    Code is packaged by feature under `com.iov42.solutions.version2.<module>.<feature>`.
    (settings.gradle, AGENTS.md)
- **Where the domain lives** (`command-service/.../command/<feature>/model`):
  - `item`: items, processes and their inputs and outputs, quantities.
  - `product`: products, variants, materials and units.
  - `supplychain`: locations and location links, supply chains, connections between
    organisations, certificates, GeoJSON.
  - `delivery`: deliveries, purchase orders and their workflows.
  - `sharing`: outbound and inbound shares and the accept-and-map flow
    (`recordsharing/inbound`), plus link sharing.
  - `dataimport`: the CSV import wizard, with the async part in
    `background-worker/worker/dataimport`.
  - `documents`, `attachment`, `tags`, `customfields`, `rulesets`, `riskassessment`.
  - `user`: organisations, users and invitations.
  - `extensions/eudrdds`: DDS statements and references.
- **API:** defined in code, not in api-designs.
  - Spring controllers in `api/.../<feature>/controller/`, documented with springdoc.
  - Every endpoint is scoped to an organisation: `/organisations/{orgId}/...`.
  - The web app reads most lists through GraphQL, in `graphql-support`.
- **Data changes:** a new Flyway migration in
  `command-service/src/main/resources/db/migration/` (`V2_<n>__<what>.sql`). Never edit an
  old one.
- **Sharing:** a field travels with a shared record only when it's marked `@ShareProperty`. A
  new field that partners should see needs one. (sharing/recordsharing/annotation)
- **Feature flags:** Spring properties, `iov42.<feature>.enabled`, read with
  `@ConditionalOnBooleanProperty`. These are separate from the web app's GrowthBook flags; a
  feature behind both needs both. (PrReviewChecklist.md)
- **Emails:** the backend raises an event, and solutions-notification-v2 sends it, through its
  client library. Template names are in `api/src/main/resources/application.properties`.
- **Tests:**
  - JUnit 5, with TestContainers (Postgres) and WireMock for integration tests.
  - A pull request's end-to-end run (`/re-run`) runs the Cypress suite in
    `iov42/cypress-bdd-pom` against the branch's deployment.
- **Conventions:** `AGENTS.md`, `PrReviewChecklist.md` and `.claude/skills/` (alt-id
  resolution, record-sharing scoping). The trunk is `main`. Branch names carry the ticket key
  and stay short.

## How work is split across the repos

- **A new or changed Interu endpoint**: the backend defines it in code, so the backend ticket
  comes first and says the request and response; then the frontend. Data the web app only
  reads may need just a GraphQL query.
- **A new or changed API in identity-platform or a newer service**: api-designs first
  (TypeSpec), then the service, then its callers.
- **Sign-in, users, organisations, connections, roles**: identity-platform, which often means
  api-designs first.
- **An email or notification**: the backend raises it, and solutions-notification-v2 sends it.
  Check whether the template exists.
- **A new record type, field or import**: the backend first (data, rules, import mapping),
  then the frontend screens, then e2e.
- **Admin-console data**: a report endpoint where the data lives, then the console.
- **New behaviour replacing something customers use**: behind a GrowthBook flag, with the
  release as its own ticket.

## How we work

- **Tickets today:** they live in Linear. Keys are `PRE-…` (team "Product & Engineering"),
  `INTH-…` and `MP-…`. Projects planned here replace Linear for their own work; reference a
  Linear key when a resource names one.
- **Branches and commits:** a frontend branch is the ticket key, and backend branch names stay
  short. Commits in the frontend go through `npm run commit` (commitizen).
- **Pull requests:** every change gets a reviewed one, with unit tests, and an end-to-end
  scenario when a user flow changes.
