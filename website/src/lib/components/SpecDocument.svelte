<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { TocEntry } from '#lib/server/spec.ts';
	import Prose from './Prose.svelte';

	interface Props {
		/** Rendered by `renderSpec` at build time from a specification in this repository. */
		html: string;
		toc: TocEntry[];
		tocLabel: string;
		/** Shown after the specification. */
		children?: Snippet;
	}

	let { html, toc, tocLabel, children }: Props = $props();
</script>

<Prose {toc} {tocLabel}>
	<div class="spec">
		<!-- eslint-disable-next-line svelte/no-at-html-tags -- rendered at build time from a specification in this repository -->
		{@html html}
	</div>
	{@render children?.()}
</Prose>

<style>
	.spec :global(.anchor) {
		float: left;
		margin-left: -1.1em;
		padding-right: 0.3em;
		color: var(--line-strong);
		text-decoration: none;
		opacity: 0;
		transition: opacity 0.15s;
	}

	.spec :global(:is(h2, h3, h4):hover .anchor),
	.spec :global(.anchor:focus) {
		opacity: 1;
	}

	.spec :global(h1 + p) {
		color: var(--ink-2);
		font-size: 1.15rem;
	}
</style>
