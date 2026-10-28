<script lang="ts">
	import ConsoleBar from '#lib/components/ConsoleBar.svelte';
	import { consoleText, fill } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const c = $derived(consoleText[i18n.lang]);
	const t = $derived(c.account);
	const megabytes = (bytes: number) => `${(bytes / 1e6).toFixed(1)} MB`;
</script>

<svelte:head>
	<title>{c.title.account} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<section class="container console">
	<ConsoleBar login={data.login} avatar={data.avatar} csrf={data.csrf} admin={data.admin} />
	<h1>{c.title.account}</h1>
	{#if data.suspended}<p class="problems" role="alert">This account is suspended.</p>{/if}
	{#if form?.deleted}<p class="note" role="status">
			{fill(t.deleted, { label: form.deleted })}
		</p>{/if}
	<p class="muted">{t.intro}</p>

	<h2>{t.clients} <span class="count">{data.clients.length} {t.of} {data.limit}</span></h2>
	{#if data.clients.length === 0}
		<p class="muted">{t.none}</p>
	{/if}
	<ul class="clients">
		{#each data.clients as client (client.id)}
			<li class="card" data-client={client.id}>
				<div>
					<strong>{client.app ?? t.manual}</strong> · {client.label}
					<br /><code>{client.id}</code>
					<small class="muted">· {c.common.created} {client.created}</small>
					{#if client.today}
						<br /><small class="muted"
							>{c.common.today}: {client.today.connections}
							{c.common.connections} · {client.today.minutes}
							{c.common.minutes} · {megabytes(client.today.bytes)}</small
						>
					{/if}
				</div>
				<form
					method="POST"
					action="?/delete"
					onsubmit={(e) => {
						if (!confirm(fill(t.confirmDelete, { label: client.label }))) e.preventDefault();
					}}
				>
					<input type="hidden" name="csrf" value={data.csrf} />
					<input type="hidden" name="client" value={client.id} />
					<button class="button danger" type="submit">{c.common.delete}</button>
				</form>
			</li>
		{/each}
	</ul>

	<h2>{t.build}</h2>
	<p>{t.buildLead} <a href={i18n.href('/developers')}>{c.title.developers} →</a></p>
</section>

<style>
	.console {
		padding-bottom: 80px;
	}
	h1 {
		font-size: clamp(1.8rem, 3vw, 2.4rem);
	}
	h2 {
		margin-top: 40px;
		font-size: 1.3rem;
	}
	.count {
		font-size: 0.9rem;
		font-weight: 400;
		color: var(--muted);
	}
	.muted {
		color: var(--muted);
	}
	.clients {
		list-style: none;
		padding: 0;
		display: grid;
		gap: 12px;
	}
	.clients li {
		display: flex;
		flex-wrap: wrap;
		gap: 12px;
		align-items: center;
		justify-content: space-between;
		padding: 16px 18px;
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.8rem;
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
	}
</style>
