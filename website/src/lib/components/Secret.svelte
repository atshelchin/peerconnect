<script lang="ts">
	// A value to copy. A secret one is shown masked (a screen can be seen or
	// captured), and one given as `load` is fetched only when it is shown or
	// copied: the page itself never holds it.
	import { consoleText } from '#lib/i18n/console.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';

	let {
		label,
		value = undefined,
		load = undefined,
		hint = undefined,
		secret = false
	}: {
		label: string;
		value?: string;
		load?: () => Promise<string>;
		hint?: string;
		secret?: boolean;
	} = $props();

	const c = $derived(consoleText[i18n.lang].common);
	let fetched: string | undefined = $state();
	let copied = $state(false);
	let revealed = $state(false);
	let failure = $state('');
	const known = $derived(value ?? fetched);

	async function get(): Promise<string | undefined> {
		if (known !== undefined) return known;
		try {
			failure = '';
			fetched = await load!();
			return fetched;
		} catch (e) {
			failure = e instanceof Error ? e.message : String(e);
		}
	}

	async function copy() {
		const text = await get();
		if (text === undefined) return;
		await navigator.clipboard.writeText(text);
		copied = true;
		setTimeout(() => (copied = false), 1500);
	}

	async function toggle() {
		if (!revealed && (await get()) === undefined) return;
		revealed = !revealed;
	}
</script>

<div class="secret">
	<span class="label">{label}</span>
	<code class="value" data-testid={label} data-value={known}
		>{(secret && !revealed) || known === undefined ? '•'.repeat(24) : known}</code
	>
	<span class="buttons">
		{#if secret}
			<button type="button" class="button ghost small" onclick={toggle}
				>{revealed ? c.hide : c.show}</button
			>
		{/if}
		<button type="button" class="button secondary small" onclick={copy}
			>{copied ? c.copied : c.copy}</button
		>
	</span>
	{#if hint}<p class="hint">{hint}</p>{/if}
	{#if failure}<p class="hint failure" role="alert">{failure}</p>{/if}
</div>

<style>
	.secret {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 6px 12px;
		align-items: center;
		border: 1px solid var(--line);
		border-radius: var(--radius-sm);
		background: var(--surface);
		padding: 12px 14px;
		margin: 10px 0;
	}
	.label {
		grid-column: 1 / -1;
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--muted);
	}
	.value {
		font-family: var(--font-mono);
		word-break: break-all;
		font-size: 0.85rem;
	}
	.hint {
		grid-column: 1 / -1;
		margin: 2px 0 0;
		font-size: 0.85rem;
		color: var(--muted);
	}
	.failure {
		color: var(--danger);
	}
	.buttons {
		display: flex;
		gap: 6px;
	}
	.small {
		min-height: 34px;
		padding: 0 14px;
		font-size: 0.85rem;
	}
</style>
