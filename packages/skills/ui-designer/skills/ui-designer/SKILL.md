---
name: ui-designer
description: Design or restyle user interfaces using vendored DESIGN.md references from popular product and brand-inspired visual systems. Use when the user asks for UI in the style of a named reference such as Stripe, Vercel, Apple, Linear, Airbnb, Spotify, Notion, Figma, or similar; asks to match a reference design; or wants the agent to choose a polished visual direction for web/app UI when no style is specified.
metadata:
  displayName: UI Designer
  icon: paintpalette
  color: cyan
  visibility: visible
  placeholder: Name a reference style or describe the UI to design
---

# UI Designer

Use this skill to create, restyle, or review UI with a visual language based on a vendored DESIGN.md reference.

The references are curated starting points, not official brand systems. Use them as inspiration for composition, tone, spacing, type, color roles, and component behavior. Do not copy logos, trademarked assets, or claim affiliation with the referenced company.

## Reference Files

- Design docs: `references/designs/<slug>.md`
- Source notice: `references/source-license.md`

Use the reference index below when the user names a reference, asks what styles are available, or gives an ambiguous brand/style name. Then load only the selected design doc.

## Style Source Contract

Every style decision must come from one vendored markdown file under `skills/ui-designer/references/designs/*.md`.

Use that vendored markdown as the sole style source for:

- composition and layout direction
- color roles and contrast
- typography and spacing
- component shape, borders, shadows, and motion

Use external pages, product docs, screenshots, and live websites for factual content only. Keep style selection anchored to the vendored markdown path above.

## Reference Index

<!-- BEGIN GENERATED DESIGN REFERENCES -->
### AI & LLM Platforms
- `claude` - Claude: Anthropic's AI assistant. Warm terracotta accent, clean editorial layout Path: `references/designs/claude.md`
- `cohere` - Cohere: Enterprise AI platform. Vibrant gradients, data-rich dashboard aesthetic Path: `references/designs/cohere.md`
- `elevenlabs` - ElevenLabs: AI voice platform. Dark cinematic UI, audio-waveform aesthetics Path: `references/designs/elevenlabs.md`
- `minimax` - Minimax: AI model provider. Bold dark interface with neon accents Path: `references/designs/minimax.md`
- `mistral.ai` - Mistral AI: Open-weight LLM provider. French-engineered minimalism, purple-toned Path: `references/designs/mistral.ai.md`
- `ollama` - Ollama: Run LLMs locally. Terminal-first, monochrome simplicity Path: `references/designs/ollama.md`
- `opencode.ai` - OpenCode AI: AI coding platform. Developer-centric dark theme Path: `references/designs/opencode.ai.md`
- `replicate` - Replicate: Run ML models via API. Clean white canvas, code-forward Path: `references/designs/replicate.md`
- `runwayml` - RunwayML: AI video generation. Cinematic dark UI, media-rich layout Path: `references/designs/runwayml.md`
- `together.ai` - Together AI: Open-source AI infrastructure. Technical, blueprint-style design Path: `references/designs/together.ai.md`
- `voltagent` - VoltAgent: AI sandbox vm. Void-black canvas, emerald accent, terminal-native Path: `references/designs/voltagent.md`
- `x.ai` - xAI: Elon Musk's AI lab. Stark monochrome, futuristic minimalism Path: `references/designs/x.ai.md`

### Automotive
- `bmw` - BMW: Luxury automotive. Dark premium surfaces, precise German engineering aesthetic Path: `references/designs/bmw.md`
- `bugatti` - Bugatti: Luxury hypercar. Cinema-black canvas, monochrome austerity, monumental display type Path: `references/designs/bugatti.md`
- `ferrari` - Ferrari: Luxury automotive. Chiaroscuro black-white editorial, Ferrari Red with extreme sparseness Path: `references/designs/ferrari.md`
- `lamborghini` - Lamborghini: Luxury automotive. True black cathedral, gold accent, LamboType custom Neo-Grotesk Path: `references/designs/lamborghini.md`
- `renault` - Renault: French automotive. Vivid aurora gradients, NouvelR proprietary typeface, zero-radius buttons Path: `references/designs/renault.md`
- `tesla` - Tesla: Electric vehicles. Radical subtraction, cinematic full-viewport photography, Universal Sans Path: `references/designs/tesla.md`

### Backend, Database & DevOps
- `clickhouse` - ClickHouse: Fast analytics database. Yellow-accented, technical documentation style Path: `references/designs/clickhouse.md`
- `composio` - Composio: Tool integration platform. Modern dark with colorful integration icons Path: `references/designs/composio.md`
- `hashicorp` - HashiCorp: Infrastructure automation. Enterprise-clean, black and white Path: `references/designs/hashicorp.md`
- `mongodb` - MongoDB: Document database. Green leaf branding, developer documentation focus Path: `references/designs/mongodb.md`
- `posthog` - PostHog: Product analytics. Playful hedgehog branding, developer-friendly dark UI Path: `references/designs/posthog.md`
- `sanity` - Sanity: Headless CMS. Red accent, content-first editorial layout Path: `references/designs/sanity.md`
- `sentry` - Sentry: Error monitoring. Dark dashboard, data-dense, pink-purple accent Path: `references/designs/sentry.md`
- `supabase` - Supabase: Open-source Firebase alternative. Dark emerald theme, code-first Path: `references/designs/supabase.md`

### Design & Creative Tools
- `airtable` - Airtable: Spreadsheet-database hybrid. Colorful, friendly, structured data aesthetic Path: `references/designs/airtable.md`
- `clay` - Clay: Creative agency. Organic shapes, soft gradients, art-directed layout Path: `references/designs/clay.md`
- `figma` - Figma: Collaborative design tool. Vibrant multi-color, playful yet professional Path: `references/designs/figma.md`
- `framer` - Framer: Website builder. Bold black and blue, motion-first, design-forward Path: `references/designs/framer.md`
- `miro` - Miro: Visual collaboration. Bright yellow accent, infinite canvas aesthetic Path: `references/designs/miro.md`
- `webflow` - Webflow: Visual web builder. Blue-accented, polished marketing site aesthetic Path: `references/designs/webflow.md`

### Developer Tools & IDEs
- `cursor` - Cursor: AI-first code editor. Sleek dark interface, gradient accents Path: `references/designs/cursor.md`
- `expo` - Expo: React Native platform. Dark theme, tight letter-spacing, code-centric Path: `references/designs/expo.md`
- `lovable` - Lovable: AI full-stack builder. Playful gradients, friendly dev aesthetic Path: `references/designs/lovable.md`
- `raycast` - Raycast: Productivity launcher. Sleek dark chrome, vibrant gradient accents Path: `references/designs/raycast.md`
- `superhuman` - Superhuman: Fast email client. Premium dark UI, keyboard-first, purple glow Path: `references/designs/superhuman.md`
- `vercel` - Vercel: Frontend deployment platform. Black and white precision, Geist font Path: `references/designs/vercel.md`
- `warp` - Warp: Modern terminal. Dark IDE-like interface, block-based command UI Path: `references/designs/warp.md`

### E-commerce & Retail
- `airbnb` - Airbnb: Travel marketplace. Warm coral accent, photography-driven, rounded UI Path: `references/designs/airbnb.md`
- `meta` - Meta: Tech retail store. Photography-first, binary light/dark surfaces, Meta Blue CTAs Path: `references/designs/meta.md`
- `nike` - Nike: Athletic retail. Monochrome UI, massive uppercase Futura, full-bleed photography Path: `references/designs/nike.md`
- `shopify` - Shopify: E-commerce platform. Dark-first cinematic, neon green accent, ultra-light display type Path: `references/designs/shopify.md`

### Fintech & Crypto
- `binance` - Binance: Crypto exchange. Bold Binance Yellow on monochrome, trading-floor urgency Path: `references/designs/binance.md`
- `coinbase` - Coinbase: Crypto exchange. Clean blue identity, trust-focused, institutional feel Path: `references/designs/coinbase.md`
- `kraken` - Kraken: Crypto trading platform. Purple-accented dark UI, data-dense dashboards Path: `references/designs/kraken.md`
- `mastercard` - Mastercard: Global payments network. Warm cream canvas, orbital pill shapes, editorial warmth Path: `references/designs/mastercard.md`
- `revolut` - Revolut: Digital banking. Sleek dark interface, gradient cards, fintech precision Path: `references/designs/revolut.md`
- `stripe` - Stripe: Payment infrastructure. Signature purple gradients, weight-300 elegance Path: `references/designs/stripe.md`
- `wise` - Wise: International money transfer. Bright green accent, friendly and clear Path: `references/designs/wise.md`

### Media & Consumer Tech
- `apple` - Apple: Consumer electronics. Premium white space, SF Pro, cinematic imagery Path: `references/designs/apple.md`
- `ibm` - IBM: Enterprise technology. Carbon design system, structured blue palette Path: `references/designs/ibm.md`
- `nvidia` - NVIDIA: GPU computing. Green-black energy, technical power aesthetic Path: `references/designs/nvidia.md`
- `pinterest` - Pinterest: Visual discovery platform. Red accent, masonry grid, image-first Path: `references/designs/pinterest.md`
- `playstation` - PlayStation: Gaming console retail. Three-surface channel layout, cyan hover-scale interaction Path: `references/designs/playstation.md`
- `spacex` - SpaceX: Space technology. Stark black and white, full-bleed imagery, futuristic Path: `references/designs/spacex.md`
- `spotify` - Spotify: Music streaming. Vibrant green on dark, bold type, album-art-driven Path: `references/designs/spotify.md`
- `theverge` - The Verge: Tech editorial media. Acid-mint and ultraviolet accents, Manuka display type Path: `references/designs/theverge.md`
- `uber` - Uber: Mobility platform. Bold black and white, tight type, urban energy Path: `references/designs/uber.md`
- `vodafone` - Vodafone: Global telecom brand. Monumental uppercase display, Vodafone Red chapter bands Path: `references/designs/vodafone.md`
- `wired` - WIRED: Tech magazine. Paper-white broadsheet density, custom serif, ink-blue links Path: `references/designs/wired.md`

### Productivity & SaaS
- `cal` - Cal.com: Open-source scheduling. Clean neutral UI, developer-oriented simplicity Path: `references/designs/cal.md`
- `intercom` - Intercom: Customer messaging. Friendly blue palette, conversational UI patterns Path: `references/designs/intercom.md`
- `linear.app` - Linear: Project management for engineers. Ultra-minimal, precise, purple accent Path: `references/designs/linear.app.md`
- `mintlify` - Mintlify: Documentation platform. Clean, green-accented, reading-optimized Path: `references/designs/mintlify.md`
- `notion` - Notion: All-in-one workspace. Warm minimalism, serif headings, soft surfaces Path: `references/designs/notion.md`
- `resend` - Resend: Email API for developers. Minimal dark theme, monospace accents Path: `references/designs/resend.md`
- `zapier` - Zapier: Automation platform. Warm orange, friendly illustration-driven Path: `references/designs/zapier.md`
<!-- END GENERATED DESIGN REFERENCES -->

## Reference Selection Priority

Pick the reference in this order:

1. Explicit style or brand in the current request.
2. Existing style already established for the same artifact or screen that the user is continuing.
3. A request-local random reference drawn from `references/designs/*.md` when the request leaves style open-ended.
4. Current-request product category, audience, and adjectives when the user gives a visual direction but no named brand.

Treat style as request-local unless the user clearly asks to keep the previous style or is obviously iterating on the same artifact. Recompute the reference from the current request each time. A brand mentioned in an earlier request does not become the default for a later unrelated request.

When the user does not mention style or UI design, or for requests like `any style is fine`, `you decide`, `pick any option`, `open-ended`, or any request that leaves style unspecified, go straight to the random-reference branch. Use product category and audience as fit checks after the random draw, and use them for one redraw only when hard constraints require it.

## When Style Is Unspecified

For a fresh artifact with no named reference and no established style, select one design doc at random from the vendored files in `references/designs/`.

Use the built-in JavaScript tool on the bundled picker. The draw source is the vendored path inside this skill, so the reference always comes from `skills/ui-designer/references/designs/*.md`.

Execution pattern:

1. Call `JavaScriptFromFile` with `path: "scripts/pick_random_reference.js"`.
2. Pass `SEED` in `env` only when you need a reproducible testcase draw.
3. Parse the returned JSON string and use `slug` as the reference.

Example tool call:

```json
{
  "path": "scripts/pick_random_reference.js",
  "env": {
    "SEED": "no-style"
  }
}
```

Selection rules:

- Treat the random draw as request-local. A brand from an earlier unrelated request does not become the default later.
- Keep the established reference when the user is clearly iterating on the same artifact and wants continuity.
- If the first random draw conflicts with hard product constraints in the current request, redraw once and keep the second result.
- Read `references/designs/<slug>.md` immediately after the draw.
- Lock the chosen markdown as the sole style source for the rest of the task.
- Complete the Reference Lock and Extraction Record before any UI design work begins.

## Reference Lock and Extraction Record

For the no-style branch, this sequence is mandatory and ordered:

1. Call `JavaScriptFromFile` on `scripts/pick_random_reference.js`.
2. Parse the returned JSON and resolve `references/designs/<slug>.md`.
3. Read that vendored markdown file in full.
4. Write a short extraction record from that file before deciding any visual direction.
5. Start implementation only after the extraction record is complete.

The extraction record must include:

- chosen slug and markdown path
- theme and atmosphere
- color roles
- typography rules
- component styling rules
- layout and spacing rules
- depth, interaction, and motion rules
- hard constraints and no-go rules

Use that extraction record as the design contract for the rest of the task.

Until the extraction record exists, keep the work in pre-design mode. Hero composition, color palette, typography, component styling, and layout direction all come after the markdown has been read and extracted.

## Reference Extraction Pass

After selecting a vendored markdown file, do one explicit extraction pass before any design work.

Read the chosen file section by section and extract concrete constraints from these headings when present:

1. `Visual Theme & Atmosphere`
2. `Color Palette & Roles`
3. `Typography Rules`
4. `Component Stylings`
5. `Layout Principles` or `Layout`
6. `Depth & Elevation`
7. `Do's and Don'ts`
8. `Responsive Behavior`

Turn that extraction into implementation-ready notes:

- theme and mood
- exact color roles and contrast
- type families, sizes, weights, tracking, and casing
- component shapes, borders, shadows, and interaction patterns
- spacing, grid, density, and section rhythm
- responsive behavior and layout collapse rules
- hard constraints to preserve while designing

If a heading is missing, continue with the remaining headings in the same vendored markdown and keep the design anchored to that file.

## Workflow

1. Identify the target UI surface, framework, whether this is a continuation of an existing artifact, and any existing design constraints from the repository.
2. Choose the reference slug from the Reference Index.
   - The only valid style source is a vendored markdown file matching `skills/ui-designer/references/designs/*.md`.
   - Use exact slug/name matches first.
   - If the current request continues the same page, screen, or feature, keep the existing reference unless the user asks for a change.
   - If the user describes a mood instead of a brand, pick the closest index entry from the current request and state the choice briefly.
   - If the style is unspecified and there is no established reference yet, call `JavaScriptFromFile` on `scripts/pick_random_reference.js`, parse the returned JSON string, and use its `slug` before any design reasoning begins.
   - For the no-style branch, product category and audience act as post-draw fit checks. They may justify one redraw when hard constraints require it.
   - After the draw, read the matching vendored file from `references/designs/<slug>.md` and lock it before making any visual decisions.
   - If the random draw clearly clashes with hard product constraints, redraw once and keep the second result.
   - If multiple references fit after an explicit style request, pick one primary reference and optionally one secondary accent reference.
3. Read `references/designs/<slug>.md` and complete the Reference Lock and Extraction Record first.
4. Do the Reference Extraction Pass from the locked markdown file.
5. Convert the extracted constraints into concrete implementation choices:
   - color tokens and semantic roles
   - typography scale, weights, and casing
   - spacing, grid, density, and breakpoints
   - button, card, input, navigation, table, and modal treatments
   - depth, borders, shadows, motion, and interaction states
6. Implement using the project's existing UI stack and component patterns.
7. Run one self-check before finishing: theme, color, type, component styling, and layout choices must each trace back to the locked markdown file.
8. Verify the result with the repo's normal build, lint, tests, screenshots, or visual checks when available.

## Design Rules

- Preserve user requirements and product functionality over reference fidelity.
- Prefer reusable tokens and existing component APIs over one-off CSS.
- Make the UI recognizably inspired by the reference without copying proprietary marks or text.
- Keep layouts responsive and stable; text must not clip or overlap.
- Use real assets only when the project already has appropriate rights or the user provides them.
- For accessibility, keep contrast, focus states, labels, keyboard behavior, and reduced-motion behavior intact.

## Output Guidance

When reporting the work, name the reference used and summarize the main visual decisions. When the style was inferred, say which reference you chose and why it fit the current request. If the requested reference was unavailable, say which closest reference was used and why.
