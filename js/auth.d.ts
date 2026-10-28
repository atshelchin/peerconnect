/** What peerconnect.dev answers for a code: the person's relay token and where to use it. */
export interface Connected {
  token_type: "peerconnect";
  /** The relay token: pass it to `connect(room, { token })`. A secret. */
  token: string;
  /** Epoch seconds. */
  expires_at: number;
  relay_url: string;
  client_id: string;
  /** The person's GitHub login, to show "connected as @login". */
  login: string;
}

export interface ConnectRequest {
  /** The URL to open in a browser. */
  url: string;
  /** Keep both until the redirect comes back: check `state`, send `verifier`. */
  state: string;
  verifier: string;
}

/** This installation's stable ID, kept in localStorage. */
export function installationId(): string;

export function connectRequest(options: {
  appId: string;
  redirectUri: string;
  installation: string;
  label?: string;
  site?: string;
}): Promise<ConnectRequest>;

export function exchangeCode(options: {
  appId: string;
  code: string;
  verifier: string;
  redirectUri: string;
  /** Only from a server. */
  appSecret?: string;
  site?: string;
}): Promise<Connected>;

/** Sends the browser to peerconnect.dev; it comes back to `redirectUri` with a code. */
export function startConnect(options: {
  appId: string;
  redirectUri: string;
  label?: string;
  site?: string;
}): Promise<void>;

/** On the redirect page: the person's token. Rejects if not reached from `startConnect`. */
export function finishConnect(options: { appId: string; url?: string; site?: string }): Promise<Connected>;
