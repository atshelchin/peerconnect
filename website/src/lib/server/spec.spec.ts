import { describe, expect, it } from 'vitest';
import groups from '../../../../GROUP.md?raw';
import spec from '../../../../SPEC.md?raw';
import { DOCUMENTS, renderSpec } from './spec';

// A link that leaves the site, stays on the page, or goes to a specification
// page; anything else would be a repository file the site does not have.
const OFF_SITE = /href="(?![#a-z]+:|#)(?!\/spec(?:\/groups)?(?:[#"]))[^"]+"/;

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
		expect(html).not.toMatch(OFF_SITE);
	});
});

describe('renderSpec on GROUP.md', () => {
	const { html, toc } = renderSpec(groups);
	const rooms = renderSpec(spec).html;

	it('anchors every numbered section', () => {
		expect(toc.length).toBeGreaterThan(20);
		expect(toc[0]).toEqual({ id: 'sec-1', label: '1. Overview', depth: 2 });
		expect(toc.map((entry) => entry.id)).toContain('sec-6');
		for (const { id } of toc) expect(html).toContain(`id="${id}"`);
	});

	it('links its own section references to sections that exist', () => {
		const targets = [...html.matchAll(/href="#(sec-[\d-]+)"/g)].map((m) => m[1]);
		expect(targets.length).toBeGreaterThan(10);
		for (const target of targets) expect(html).toContain(`id="${target}"`);
	});

	it('links references into SPEC.md to sections of the specification page', () => {
		const targets = [...html.matchAll(/href="\/spec#(sec-[\d-]+)"/g)].map((m) => m[1]);
		expect(targets).toEqual(expect.arrayContaining(['sec-4-1', 'sec-4-6', 'sec-6-1']));
		for (const target of targets) expect(rooms).toContain(`id="${target}"`);
		expect(html).toContain('<a href="/spec#sec-4-1">SPEC.md §4.1</a>');
		// Not to this page's own §4.1.
		expect(html).not.toContain('SPEC.md <a href="#sec-4-1">');
	});

	it('links SPEC.md to its page, and other files to nowhere off the site', () => {
		expect(DOCUMENTS['SPEC.md']).toBe('/spec');
		expect(html).toContain('<a href="/spec">SPEC.md</a>');
		expect(html).not.toMatch(OFF_SITE);
	});
});
