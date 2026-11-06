const REFERENCE_SLUGS = [
  "airbnb",
  "airtable",
  "apple",
  "binance",
  "bmw",
  "bugatti",
  "cal",
  "claude",
  "clay",
  "clickhouse",
  "cohere",
  "coinbase",
  "composio",
  "cursor",
  "elevenlabs",
  "expo",
  "ferrari",
  "figma",
  "framer",
  "hashicorp",
  "ibm",
  "intercom",
  "kraken",
  "lamborghini",
  "linear.app",
  "lovable",
  "mastercard",
  "meta",
  "minimax",
  "mintlify",
  "miro",
  "mistral.ai",
  "mongodb",
  "nike",
  "notion",
  "nvidia",
  "ollama",
  "opencode.ai",
  "pinterest",
  "playstation",
  "posthog",
  "raycast",
  "renault",
  "replicate",
  "resend",
  "revolut",
  "runwayml",
  "sanity",
  "sentry",
  "shopify",
  "spacex",
  "spotify",
  "stripe",
  "supabase",
  "superhuman",
  "tesla",
  "theverge",
  "together.ai",
  "uber",
  "vercel",
  "vodafone",
  "voltagent",
  "warp",
  "webflow",
  "wired",
  "wise",
  "x.ai",
  "zapier",
];

function seededIndex(seed, size) {
  let hash = 2166136261;
  for (let i = 0; i < seed.length; i += 1) {
    hash ^= seed.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0) % size;
}

const seed = typeof env === "object" && typeof env.SEED === "string" ? env.SEED : "";
const index = seed ? seededIndex(seed, REFERENCE_SLUGS.length) : Math.floor(Math.random() * REFERENCE_SLUGS.length);

JSON.stringify({
  slug: REFERENCE_SLUGS[index],
  reference_count: REFERENCE_SLUGS.length,
  ...(seed ? { seed } : {})
});
