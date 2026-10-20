import { renderSpec } from '#lib/server/spec.ts';
import spec from '../../../../SPEC.md?raw';
import type { PageServerLoad } from './$types';

// The specification is normative and exists in English only.
export const load: PageServerLoad = () => renderSpec(spec);
