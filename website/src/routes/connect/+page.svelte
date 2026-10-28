<script lang="ts">
	/* eslint svelte/no-at-html-tags: "off" -- fillHtml escapes every value; only our own markup remains */
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { consoleText, fill, fillHtml } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const k = $derived(consoleText[i18n.lang].connect);

	// The app's request travels with the form: `?/approve` alone would drop
	// it. Kit finds the action by its `/approve` key wherever it stands.
	const approveAction = $derived(`${page.url.search}&/approve`);
	const app = $derived(data.app.name);

	// Back to the app as soon as the code exists; the button is there for a
	// browser that does not follow.
	onMount(() => {
		if (form?.done) window.location.assign(form.done.returnTo);
	});
</script>

<svelte:head>
	<title>{consoleText[i18n.lang].title.connect} · PeerConnect</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<section class="container connect">
	{#if form?.limit}
		<h1>{fill(k.heading, { app })}</h1>
		<p class="problems" role="alert">
			{fill(k.limit, { limit: form.limit })}
			<a href={i18n.href('/account')}>{consoleText[i18n.lang].title.account}</a>
		</p>
	{:else if form?.done}
		<h1>{k.done}</h1>
		<p>{fill(k.returning, { app: form.done.appName })}</p>
		<!-- The app's own address (a registered redirect URI), not a route of this site. -->
		<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
		<p>
			<a class="button primary" href={form.done.returnTo}
				>{fill(k.returnTo, { app: form.done.appName })}</a
			>
		</p>
	{:else}
		<h1>{fill(k.heading, { app })}</h1>
		<p class="lead">{@html fillHtml(k.lead, { app, label: data.app.label, login: data.login })}</p>
		<p class="muted">{fill(k.detail, { app })}</p>
		<form method="POST" action={approveAction}>
			<input type="hidden" name="csrf" value={data.csrf} />
			{#if data.choices.length}
				<fieldset class="choices">
					<legend>{k.as}</legend>
					{#each data.choices as c (c.id)}
						<label class="choice">
							<input type="radio" name="client" value={c.id} checked={c.here} />
							<span>
								<strong>{c.label}</strong>
								<code>{c.id}</code><br />
								<small
									>{c.here
										? fill(k.thisOne, { app })
										: c.held
											? fill(k.inUse, { app })
											: app}</small
								>
							</span>
						</label>
					{/each}
					<label class="choice">
						<input
							type="radio"
							name="client"
							value="new"
							checked={!data.choices.some((c) => c.here)}
						/>
						<span><strong>{k.fresh}</strong></span>
					</label>
				</fieldset>
			{/if}
			<button class="button primary" type="submit">{k.approve}</button>
		</form>
	{/if}
</section>

<style>
	.connect {
		max-width: 680px;
		padding-block: 64px;
	}
	.lead {
		font-size: 1.1rem;
	}
	.muted,
	small {
		color: var(--muted);
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.8rem;
		margin-left: 6px;
	}
	.choices {
		border: 1px solid var(--line);
		border-radius: var(--radius);
		padding: 8px 16px;
		margin: 20px 0;
		min-width: 0;
	}
	.choices legend {
		padding: 0 6px;
		color: var(--muted);
	}
	.choice {
		display: flex;
		gap: 10px;
		align-items: flex-start;
		margin-block: 12px;
		cursor: pointer;
	}
	.choice input {
		margin-top: 5px;
	}
	.problems {
		border: 1px solid var(--danger);
		color: var(--danger);
		border-radius: var(--radius-sm);
		padding: 10px 14px;
	}
</style>
