<script lang="ts">
	import type { CodeLang } from '#lib/highlight.ts';
	import CodeBlock from './CodeBlock.svelte';

	interface Tab {
		label: string;
		code: string;
		lang: CodeLang;
		title?: string;
	}

	let { tabs, id }: { tabs: Tab[]; id: string } = $props();
	let active = $state(0);

	// Arrow keys move between tabs, as in the WAI-ARIA tabs pattern.
	function onkeydown(event: KeyboardEvent) {
		const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
		if (!step) return;
		active = (active + step + tabs.length) % tabs.length;
		document.getElementById(`${id}-tab-${active}`)?.focus();
	}
</script>

<div class="tabs">
	<div class="list" role="tablist" tabindex="-1" {onkeydown}>
		{#each tabs as tab, i (tab.label)}
			<button
				type="button"
				role="tab"
				id="{id}-tab-{i}"
				aria-selected={i === active}
				aria-controls="{id}-panel-{i}"
				tabindex={i === active ? 0 : -1}
				onclick={() => (active = i)}
			>
				{tab.label}
			</button>
		{/each}
	</div>
	{#each tabs as tab, i (tab.label)}
		<div role="tabpanel" id="{id}-panel-{i}" aria-labelledby="{id}-tab-{i}" hidden={i !== active}>
			<CodeBlock code={tab.code} lang={tab.lang} title={tab.title} />
		</div>
	{/each}
</div>

<style>
	.list {
		display: inline-flex;
		gap: 4px;
		margin-bottom: 12px;
		padding: 4px;
		border-radius: 999px;
		background: var(--bg-soft);
		border: 1px solid var(--line);
	}

	button {
		min-height: 36px;
		padding: 0 16px;
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--muted);
		font: inherit;
		font-size: 0.9rem;
		font-weight: 600;
		cursor: pointer;
	}

	button[aria-selected='true'] {
		background: var(--surface);
		color: var(--ink);
		box-shadow: 0 1px 3px rgb(0 0 0 / 0.08);
	}
</style>
