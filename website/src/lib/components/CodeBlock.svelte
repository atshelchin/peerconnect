<script lang="ts">
	import { highlight, type CodeLang } from '#lib/highlight.ts';
	import { i18n } from '#lib/i18n/current.svelte.ts';

	interface Props {
		code: string;
		lang?: CodeLang;
		title?: string;
	}

	let { code, lang = 'text', title }: Props = $props();
	let copied = $state(false);

	const html = $derived(highlight(code, lang));

	async function copy() {
		try {
			await navigator.clipboard.writeText(code);
			copied = true;
			setTimeout(() => (copied = false), 1600);
		} catch {
			// clipboard not available
		}
	}
</script>

<div class="code">
	<div class="bar">
		<span class="title">{title ?? lang}</span>
		<button type="button" class="copy" onclick={copy} aria-live="polite">
			{copied ? i18n.t.docs.copied : i18n.t.docs.copy}
		</button>
	</div>
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- produced by highlight(), which escapes the code -->
	<pre><code>{@html html}</code></pre>
</div>

<style>
	.code {
		overflow: hidden;
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		border: 1px solid var(--code-line);
		color: var(--code-ink);
	}

	.bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 6px 8px 6px 16px;
		border-bottom: 1px solid var(--code-line);
	}

	.title {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--code-muted);
	}

	.copy {
		padding: 4px 10px;
		border: 1px solid var(--code-line);
		border-radius: 6px;
		background: transparent;
		color: var(--code-muted);
		font: inherit;
		font-size: 0.75rem;
		cursor: pointer;
	}

	.copy:hover {
		color: var(--code-ink);
		border-color: var(--code-muted);
	}

	pre {
		margin: 0;
		padding: 16px 18px 18px;
		overflow-x: auto;
		font-size: 0.84rem;
		line-height: 1.7;
		tab-size: 4;
	}

	code {
		font-size: inherit;
	}
</style>
