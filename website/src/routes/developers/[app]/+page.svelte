<script lang="ts">
	import AppForm from '#lib/components/AppForm.svelte';
	import CodeBlock from '#lib/components/CodeBlock.svelte';
	import ConsoleBar from '#lib/components/ConsoleBar.svelte';
	import Secret from '#lib/components/Secret.svelte';
	import { consoleText, fill } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { SITE_URL } from '#lib/site.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const c = $derived(consoleText[i18n.lang]);
	const a = $derived(c.app);

	async function secret(): Promise<string> {
		const response = await fetch(`/developers/${data.app.id}/secret`, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ csrf: data.csrf })
		});
		if (!response.ok) throw new Error(`The server said ${response.status}. Please reload.`);
		return ((await response.json()) as { secret: string }).secret;
	}

	const confirmed = (text: string) => (e: SubmitEvent) => {
		if (!confirm(text)) e.preventDefault();
	};

	const megabytes = (bytes: number) => `${(bytes / 1e6).toFixed(1)} MB`;

	const webSnippet = $derived(`# 1. ${a.stepOpen}
${SITE_URL}/connect?app=${data.app.id}&redirect_uri=${encodeURIComponent(data.redirectUri)}&state=<state>&code_challenge=<S256 challenge>&code_challenge_method=S256&installation=<installation ID>

# 2. ${a.stepBack}
#    ${data.redirectUri}?code=<code>&state=<state>
curl ${SITE_URL}/api/token -H "content-type: application/json" -d '{
  "grant_type": "authorization_code", "client_id": "${data.app.id}",
  "code": "<code>", "code_verifier": "<verifier>", "redirect_uri": "${data.redirectUri}"
}'
# → {"token": "…", "relay_url": "${data.relayUrl}", "login": "…", …}`);

	const serverSnippet = $derived(`curl ${SITE_URL}/api/apps/${data.app.id}/tokens \\
  -H "authorization: Bearer $PEERCONNECT_APP_SECRET" \\
  -H "content-type: application/json" \\
  -d '{"user": "user-42"}'
# → {"token": "…", "expires_at": …, "relay_url": "${data.relayUrl}", "client_id": "c-…"}
# The same user gets the same token; add "rotate": true for a new one.
# DELETE ${SITE_URL}/api/apps/${data.app.id}/users/user-42 stops it.`);
</script>

<svelte:head>
	<title>{data.app.name} · {c.title.developers} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<section class="container console">
	<ConsoleBar login={data.login} avatar={data.avatar} csrf={data.csrf} admin={data.admin} />
	<p><a href={i18n.href('/developers')}>← {a.back}</a></p>

	<h1>{data.app.name}</h1>
	{#if form?.saved}<p class="note" role="status">{a.saved}</p>{/if}
	{#if form?.rotated}<p class="note" role="status">{a.rotated}</p>{/if}
	{#if form?.more}<p class="problems" role="alert">{fill(a.more, { n: form.more })}</p>{/if}

	<h2>{a.credentials}</h2>
	<Secret label={a.appId} value={data.app.id} hint={a.appIdHint} />
	<Secret label={a.appSecret} load={secret} secret hint={a.appSecretHint} />
	<form method="POST" action="?/rotate" onsubmit={confirmed(a.confirmRotate)}>
		<input type="hidden" name="csrf" value={data.csrf} />
		<button class="button ghost" type="submit">{a.rotate}</button>
	</form>

	<h2>{a.users}</h2>
	<p>
		<strong>{data.counts.people}</strong>
		{a.people} · <strong>{data.counts.users}</strong>
		{a.serverUsers}
	</p>
	{#if data.usage}
		<p class="muted">
			{c.common.last30}: {data.usage.connections}
			{c.common.connections} · {data.usage.minutes}
			{c.common.minutes} · {megabytes(data.usage.bytes)}
			{c.common.traffic}
		</p>
	{/if}

	<h2>{a.integrate}</h2>
	<h3>① {a.pathA}</h3>
	<p>{a.pathALead} <a href="/docs/api#sec-4">API §4</a></p>
	<CodeBlock code={webSnippet} lang="sh" title="OAuth + PKCE" />
	<h3>② {a.pathB}</h3>
	<p>{a.pathBLead} <a href="/docs/api#sec-5">API §5</a></p>
	<CodeBlock code={serverSnippet} lang="sh" title="server" />

	<h3>{a.testToken}</h3>
	<p>{a.testTokenLead}</p>
	{#if form?.test}
		<Secret label={fill(a.tokenFor, { app: data.app.name })} value={form.test.token} secret />
	{:else}
		<form method="POST" action="?/test">
			<input type="hidden" name="csrf" value={data.csrf} />
			<button class="button secondary" type="submit">{a.makeToken}</button>
		</form>
	{/if}

	<h2>{a.settings}</h2>
	{#if form?.problems}
		<div class="problems" role="alert">
			<strong>{c.developers.invalid}</strong>
			<ul>
				{#each form.problems as problem (problem)}<li>{problem}</li>{/each}
			</ul>
		</div>
	{/if}
	<AppForm action="?/save" csrf={data.csrf} submit={c.common.save} values={data.values} />

	<h2>{a.danger}</h2>
	<p class="muted">{a.dangerLead}</p>
	<form
		method="POST"
		action="?/delete"
		onsubmit={confirmed(fill(a.confirmDelete, { app: data.app.name }))}
	>
		<input type="hidden" name="csrf" value={data.csrf} />
		<button class="button danger" type="submit">{a.danger}</button>
	</form>
</section>

<style>
	.console {
		padding-bottom: 80px;
	}
	h1 {
		font-size: clamp(1.8rem, 3vw, 2.4rem);
	}
	h2 {
		margin-top: 44px;
		font-size: 1.3rem;
	}
	h3 {
		margin-top: 28px;
		font-size: 1.05rem;
	}
	.muted {
		color: var(--muted);
	}
	.note {
		border: 1px solid var(--accent);
		background: var(--accent-soft);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
	}
	.problems {
		border: 1px solid var(--danger);
		color: var(--danger);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
		margin-bottom: 16px;
	}
	.problems ul {
		margin: 6px 0 0;
		padding-left: 20px;
	}
</style>
