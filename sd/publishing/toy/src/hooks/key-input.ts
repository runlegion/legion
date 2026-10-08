import type * as React from 'react';
import type { KeyInput } from '@/lib/contract';

/**
 * Translate a React keyboard event into the contract's KeyInput -- the one
 * React-adapter seam a controller needs to feed `spec.keymap`. The DOM-native
 * bindings build their own KeyInput from a native KeyboardEvent; this is the
 * React side of that same translation.
 */
export function keyInputOf(event: React.KeyboardEvent): KeyInput {
  return {
    key: event.key,
    shiftKey: event.shiftKey,
    ctrlKey: event.ctrlKey,
    altKey: event.altKey,
    metaKey: event.metaKey,
  };
}
