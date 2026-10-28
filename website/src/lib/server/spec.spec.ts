import { describe, expect, it } from 'vitest';
import spec from '../../../../SPEC.md?raw';
import { renderSpec } from './spec';

describe('renderSpec', () => {
	const { html, toc } = renderSpec(spec);

	it('anchors every numbered section', () => {
		expect(toc.length).toBeGreaterThan(20);
		expect(toc[0]).toEqual({ id: 'sec-1', label: '1. Overview', depth: 2 });
		for (const { id } of toc) expect(html).toContain(`id="${id}"`);
	});

	it('links section references to sections that exist', () => {
		const targets = [...html.matchAll(/href="#(sec-[\d-]+)"/g)].map((m) => m[1]);
		expect(targets.length).toBeGreaterThan(10);
		for (const target of targets) expect(html).toContain(`id="${target}"`);
	});

	it('does not link references into other documents', () => {
		expect(html).toContain('RFC 4648 §5');
		expect(html).not.toMatch(/RFC 4648 <a/);
	});

	it('leaves no links to files that are not on the site', () => {
		expect(html).not.toMatch(/href="(?![#a-z]+:|#)[^"]+"/);
	});
});
