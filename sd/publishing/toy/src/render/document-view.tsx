import { CompositeFileSchema } from "@/composites/runtime/manifest";
import { Composite } from "@/composites/runtime/to-jsx";
import { expandComposite } from "@/bind/expand";
import { components, MissingBlock } from "@/render/components";

/**
 * One document through one composite: validate the composite file with
 * rafters' own schema, run the binding pass, hand the plain blocks to
 * rafters' renderer.
 */
export function DocumentView({ composite, payload }: { composite: unknown; payload: unknown }) {
  const file = CompositeFileSchema.parse(composite);
  const blocks = expandComposite(file, payload);
  return <Composite blocks={blocks} components={components} fallback={MissingBlock} />;
}
