/** The site's public origin, used for canonical URLs, hreflang and the sitemap. */
export const SITE_URL = 'https://peerconnect.dev';

/** The public relay run for testing: two-peer rooms, over WebSockets (a Cloudflare Worker). */
export const RELAY_URL = 'wss://relay.peerconnect.dev';

/** The public group relay and the live voice and video demo: the Linux relay, over QUIC and WebTransport. */
export const GROUP_RELAY_URL = 'https://group-relay.peerconnect.dev';

/** The source repository. Set it to null to hide every link to it. */
export const REPO_URL: string | null = 'https://github.com/atshelchin/peerconnect';

export const SPEC_VERSION = 'v1';
