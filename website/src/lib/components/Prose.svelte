<script lang="ts">
	import { onMount, type Snippet } from 'svelte';

	interface Props {
		toc: { id: string; label: string; depth?: number }[];
		tocLabel: string;
		children: Snippet;
	}

	let { toc, tocLabel, children }: Props = $props();
	let current = $state('');

	// Highlights the section being read.
	onMount(() => {
		const headings = toc
			.map((item) => document.getElementById(item.id))
			.filter((el): el is HTMLElement => el !== null);
		const observer = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) if (entry.isIntersecting) current = entry.target.id;
			},
			{ rootMargin: '-80px 0px -70% 0px' }
		);
		headings.forEach((heading) => observer.observe(heading));
		return () => observer.disconnect();
	});
</script>

<div class="container layout">
	<aside class="toc">
		<nav aria-label={tocLabel}>
			<p class="toc-title">{tocLabel}</p>
			<ol>
				{#each toc as item (item.id)}
					<li class:sub={(item.depth ?? 2) > 2}>
						<a href="#{item.id}" aria-current={current === item.id ? 'location' : undefined}>
							{item.label}
						</a>
					</li>
				{/each}
			</ol>
		</nav>
	</aside>
	<article class="prose">
		{@render children()}
	</article>
</div>

<style>
	.layout {
		display: grid;
		grid-template-columns: 220px minmax(0, 1fr);
		gap: clamp(32px, 5vw, 72px);
		padding-block: clamp(40px, 6vw, 72px) clamp(64px, 9vw, 112px);
	}

	.toc nav {
		position: sticky;
		top: calc(var(--header-h) + 24px);
		max-height: calc(100vh - var(--header-h) - 48px);
		overflow-y: auto;
	}

	.toc-title {
		margin-bottom: 10px;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--muted);
	}

	.toc ol {
		margin: 0;
		padding: 0;
		list-style: none;
		border-left: 1px solid var(--line);
	}

	.toc a {
		display: block;
		margin-left: -1px;
		padding: 5px 0 5px 14px;
		border-left: 2px solid transparent;
		color: var(--muted);
		font-size: 0.9rem;
		line-height: 1.4;
		text-decoration: none;
	}

	.toc .sub a {
		padding-left: 26px;
		font-size: 0.85rem;
	}

	.toc a:hover {
		color: var(--ink);
	}

	.toc a[aria-current='location'] {
		border-left-color: var(--accent);
		color: var(--accent);
		font-weight: 600;
	}

	.prose {
		max-width: 760px;
		min-width: 0;
	}

	.prose :global(h1) {
		margin-bottom: 18px;
		font-size: clamp(2.1rem, 4.6vw, 3rem);
	}

	.prose :global(.lead) {
		margin-bottom: 8px;
		color: var(--ink-2);
		font-size: 1.15rem;
	}

	.prose :global(h2) {
		margin: 56px 0 16px;
		padding-top: 24px;
		border-top: 1px solid var(--line);
		font-size: clamp(1.5rem, 2.8vw, 1.9rem);
	}

	.prose :global(h3) {
		margin: 32px 0 12px;
		font-size: 1.18rem;
	}

	.prose :global(h4) {
		margin: 24px 0 8px;
		font-size: 1.02rem;
	}

	.prose :global(:is(p, ul, ol, dl, .code, .table, table, pre, blockquote)) {
		margin-top: 0;
		margin-bottom: 18px;
	}

	.prose :global(:is(ul, ol)) {
		padding-left: 1.3em;
	}

	.prose :global(li) {
		margin-bottom: 8px;
	}

	.prose :global(li::marker) {
		color: var(--muted);
	}

	.prose :global(dt) {
		margin-top: 18px;
		font-weight: 650;
	}

	.prose :global(dd) {
		margin: 4px 0 0;
		color: var(--ink-2);
	}

	.prose :global(.table) {
		overflow-x: auto;
	}

	.prose :global(table) {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.94rem;
	}

	.prose :global(:is(th, td)) {
		padding: 10px 12px;
		border-bottom: 1px solid var(--line);
		text-align: left;
		vertical-align: top;
	}

	.prose :global(th) {
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--muted);
		background: var(--bg-soft);
	}

	.prose :global(td code) {
		white-space: nowrap;
	}

	.prose :global(pre:not(.code pre)) {
		padding: 16px 18px;
		overflow-x: auto;
		border-radius: var(--radius-sm);
		background: var(--code-bg);
		color: var(--code-ink);
		font-size: 0.84rem;
		line-height: 1.65;
	}

	.prose :global(blockquote) {
		margin-left: 0;
		padding: 4px 0 4px 18px;
		border-left: 3px solid var(--line-strong);
		color: var(--ink-2);
	}

	.prose :global(.next) {
		margin-top: 48px;
		padding: 18px 22px;
		border-radius: var(--radius-sm);
		background: var(--accent-soft);
	}

	.prose :global(:is(h1, h2, h3, h4)[id]) {
		scroll-margin-top: calc(var(--header-h) + 16px);
	}

	@media (max-width: 900px) {
		.layout {
			grid-template-columns: 1fr;
		}

		.toc {
			display: none;
		}
	}
</style>
