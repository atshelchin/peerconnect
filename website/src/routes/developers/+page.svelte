<script lang="ts">
	import AppForm from '#lib/components/AppForm.svelte';
	import ConsoleBar from '#lib/components/ConsoleBar.svelte';
	import { consoleText, fill } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const c = $derived(consoleText[i18n.lang]);
	const d = $derived(c.developers);
</script>

<svelte:head>
	<title>{c.title.developers} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<section class="container console">
	<ConsoleBar login={data.login} avatar={data.avatar} csrf={data.csrf} admin={data.admin} />
	<h1>{c.title.developers}</h1>
	<p class="lead">{d.lead}</p>

	<h2>{d.apps}</h2>
	{#if data.apps.length === 0}
		<p class="muted">{d.none}</p>
	{:else}
		<ul class="apps">
			{#each data.apps as a (a.id)}
				<li class="card">
					<a href={i18n.href(`/developers/${a.id}`)}><strong>{a.name}</strong></a>
					<code>{a.id}</code>
					<span class="muted"
						>{a.people} · {c.app.people} &nbsp;|&nbsp; {a.users} · {c.app.serverUsers}</span
					>
				</li>
			{/each}
		</ul>
	{/if}

	<h2>{d.create}</h2>
	{#if form?.problems}
		<div class="problems" role="alert">
			<strong>{d.invalid}</strong>
			<ul>
				{#each form.problems as problem (problem)}<li>{problem}</li>{/each}
			</ul>
		</div>
	{/if}
	{#if form?.limit}<p class="problems" role="alert">{fill(d.limit, { limit: form.limit })}</p>{/if}
	<AppForm action="?/create" csrf={data.csrf} submit={d.register} values={form?.values} />
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
	.lead,
	.muted {
		color: var(--muted);
	}
	.lead {
		max-width: 720px;
	}
	.apps {
		list-style: none;
		padding: 0;
		display: grid;
		gap: 12px;
	}
	.apps li {
		display: grid;
		gap: 4px;
		padding: 16px 18px;
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.85rem;
		word-break: break-all;
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
