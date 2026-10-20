<script lang="ts">
	import type { Dict } from '#lib/i18n/en.ts';

	let { t }: { t: Dict['home']['diagram'] } = $props();

	// What the relay sees: ChaCha20-Poly1305 output, indistinguishable from noise.
	const cipher = '9f2c e107 3ab4 77d0 c5e1 08fa 2b6e 913c d4a7 5e02 f81b 6c39 a0d5 47e8 1f93 bb20 ';
</script>

<figure class="diagram" aria-label={t.label}>
	<div class="track" aria-hidden="true">
		<div class="node peer">
			<span class="avatar">A</span>
			<span class="name">{t.peerA}</span>
			<span class="bubble">{t.plaintext}</span>
		</div>
		<div class="link first">
			<i class="packet"></i><i class="packet"></i><i class="packet"></i>
		</div>
		<div class="node relay">
			<span class="name">{t.relay}</span>
			<span class="sees">{t.sees}</span>
			<span class="cipher"><span class="scroll">{cipher}{cipher}</span></span>
		</div>
		<div class="link second">
			<i class="packet"></i><i class="packet"></i><i class="packet"></i>
		</div>
		<div class="node peer">
			<span class="avatar b">B</span>
			<span class="name">{t.peerB}</span>
			<span class="bubble">{t.plaintext}</span>
		</div>
	</div>
	<figcaption>{t.caption}</figcaption>
</figure>

<style>
	.diagram {
		margin: 0;
		padding: clamp(20px, 3vw, 32px);
		border: 1px solid var(--line);
		border-radius: 24px;
		background:
			radial-gradient(circle at 50% 0%, var(--accent-soft), transparent 70%), var(--surface);
		box-shadow: var(--shadow);
	}

	.track {
		display: flex;
		align-items: center;
	}

	.node {
		display: grid;
		justify-items: center;
		gap: 8px;
		flex: none;
	}

	.peer {
		width: 96px;
	}

	.avatar {
		display: grid;
		place-items: center;
		width: 52px;
		height: 52px;
		border-radius: 16px;
		background: var(--accent);
		color: var(--accent-ink);
		font-weight: 700;
		font-size: 1.2rem;
	}

	.name {
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--ink-2);
	}

	.bubble {
		padding: 4px 12px;
		border-radius: 12px;
		background: var(--accent-soft);
		color: var(--accent-strong);
		font-family: var(--font-mono);
		font-size: 0.85rem;
		white-space: nowrap;
	}

	.relay {
		width: clamp(132px, 22vw, 184px);
		padding: 14px 12px;
		border: 1px dashed var(--line-strong);
		border-radius: 16px;
		background: var(--bg-soft);
	}

	.sees {
		font-size: 0.72rem;
		color: var(--muted);
	}

	.cipher {
		width: 100%;
		overflow: hidden;
		border-radius: 8px;
		background: var(--cipher-soft);
		mask-image: linear-gradient(90deg, transparent, #000 18%, #000 82%, transparent);
	}

	.scroll {
		display: inline-block;
		padding: 4px 0;
		color: var(--cipher);
		font-family: var(--font-mono);
		font-size: 0.8rem;
		white-space: nowrap;
		animation: scroll 14s linear infinite;
	}

	.link {
		position: relative;
		flex: 1;
		height: 2px;
		margin-inline: 6px;
		background: repeating-linear-gradient(90deg, var(--line-strong) 0 6px, transparent 6px 12px);
		container-type: inline-size;
	}

	.packet {
		position: absolute;
		top: -4px;
		left: 0;
		width: 10px;
		height: 10px;
		border-radius: 3px;
		background: var(--cipher);
		opacity: 0;
		animation: travel 3.6s cubic-bezier(0.45, 0, 0.55, 1) infinite;
	}

	.packet:nth-child(2) {
		animation-delay: 1.2s;
	}

	.packet:nth-child(3) {
		animation-delay: 2.4s;
	}

	.second .packet {
		animation-name: travel-late;
	}

	figcaption {
		margin-top: 20px;
		color: var(--muted);
		font-size: 0.92rem;
		text-align: center;
	}

	@keyframes travel {
		0% {
			transform: translateX(0);
			opacity: 0;
		}
		8% {
			opacity: 1;
		}
		42% {
			opacity: 1;
		}
		50% {
			transform: translateX(calc(100cqw - 10px));
			opacity: 0;
		}
		100% {
			transform: translateX(calc(100cqw - 10px));
			opacity: 0;
		}
	}

	/* The relay forwards each packet once it has arrived. */
	@keyframes travel-late {
		0%,
		50% {
			transform: translateX(0);
			opacity: 0;
		}
		58% {
			opacity: 1;
		}
		92% {
			opacity: 1;
		}
		100% {
			transform: translateX(calc(100cqw - 10px));
			opacity: 0;
		}
	}

	@keyframes scroll {
		to {
			transform: translateX(-50%);
		}
	}

	/* Narrow screens: the message travels downwards. */
	@keyframes drop {
		0% {
			transform: translateY(0);
			opacity: 0;
		}
		8%,
		42% {
			opacity: 1;
		}
		50%,
		100% {
			transform: translateY(calc(100cqh - 10px));
			opacity: 0;
		}
	}

	@keyframes drop-late {
		0%,
		50% {
			transform: translateY(0);
			opacity: 0;
		}
		58%,
		92% {
			opacity: 1;
		}
		100% {
			transform: translateY(calc(100cqh - 10px));
			opacity: 0;
		}
	}

	@media (max-width: 560px) {
		.track {
			flex-direction: column;
		}

		.peer {
			width: auto;
			grid-auto-flow: column;
			align-items: center;
			gap: 12px;
		}

		.avatar {
			width: 44px;
			height: 44px;
			border-radius: 14px;
		}

		.relay {
			width: min(100%, 280px);
		}

		.link {
			flex: none;
			width: 2px;
			height: 44px;
			margin: 8px 0;
			background: repeating-linear-gradient(180deg, var(--line-strong) 0 6px, transparent 6px 12px);
			container-type: size;
		}

		.packet {
			top: 0;
			left: -4px;
			animation-name: drop;
		}

		.second .packet {
			animation-name: drop-late;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.packet {
			display: none;
		}

		.scroll {
			animation: none;
		}
	}
</style>
