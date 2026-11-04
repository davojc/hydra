# What we build, and where

**Keep this file true.** It's the planning session's picture of the product. When it's wrong
or missing something, change it in a pull request to this repo: every planning session after
that uses the new version. When a project depends on something not described here, ask a
question rather than guess.

## The product

**Interu**, by iov42: a platform for supply-chain traceability and compliance.

- Companies record what they make, buy and move: **items**, **locations**, **processes**
  (inputs to outputs), **deliveries** and **purchase orders**, with **documents** and
  **certificates** attached. Records come in by hand or by **CSV import**.
- They **share** records with partners along the supply chain. A partner accepts a share and
  **maps** its records onto their own: link to one they have, or add it as new.
- **EUDR**: Interu prepares **due diligence statements (DDS)** for the EU Deforestation
  Regulation from that traced data, including geolocation (GPS) of origin.
- **Product groups** organise items for sharing and reporting.
- Users belong to **organisations**; an **admin console** manages them.

Who uses it: producers, traders and brands in regulated supply chains; our Customer Success
(CS) team supports them. Plain words for them: "record", "share", "partner", "statement".

## The repos

| Repo | What's there |
| --- | --- |
| `iov42/version2-frontend` | The Interu web app: Angular, unit tests in Vitest, end-to-end tests in Playwright (`e2e/`). Copy is translated: English source strings only (`messages.xlf`), ICU messages for plurals. |
| `iov42/version2-backend` | The Interu backend and API: Java, built and tested in CI (and Docker). Data model, sharing, mapping, imports, DDS. |
| `iov42/identity-platform` | Sign-in, organisations and sessions (.NET). |
| `iov42/admin-console` | The internal admin console. |
| `iov42/svc-usage` | Usage and metering service (.NET). |
| `iov42/svc-document` | Document storage and scanning (.NET). |
| `iov42/api-designs` | API designs, agreed before building. |

A change that touches the API usually needs a backend ticket before the frontend one, and an
api-designs change before both when the API's shape changes.

## How we work

- Tickets today live in Linear (team "Product & Engineering", keys `PRE-…`); this repo's
  projects replace that for the work planned here. Reference a PRE ticket when a resource
  names one.
- Frontend branches are named after the ticket key; backend branch names stay short.
- New behaviour ships behind a feature flag when it replaces something customers use today.
- Every change gets a reviewed pull request; frontend changes come with unit tests, and
  end-to-end tests when a user flow changes.
