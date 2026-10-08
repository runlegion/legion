/**
 * Status message component for important user feedback
 *
 * @cognitive-load 3/10 - Simple message display with clear visual hierarchy
 * @attention-economics Variant hierarchy: destructive=immediate attention, warning=caution, success=confirmation, info=supplementary
 * @trust-building Clear, honest feedback builds confidence; destructive alerts require careful wording
 * @accessibility role="alert" for urgent messages; role="status" for informational; never color-only
 * @semantic-meaning Variant mapping: default=neutral, info=helpful context, success=positive confirmation, warning=proceed with caution, destructive=error or danger
 *
 * @usage-patterns
 * DO: Use destructive for errors that need user action
 * DO: Use success to confirm completed actions
 * DO: Use warning for potential issues before they happen
 * DO: Include icons to reinforce meaning beyond color
 * NEVER: Use alerts for transient feedback (use contextual feedback instead)
 * NEVER: Stack multiple alerts - prioritize the most important
 * NEVER: Use destructive for warnings or warnings for info
 *
 * @example
 * ```tsx
 * // Error alert
 * <Alert variant="destructive">
 *   <AlertCircle className="h-4 w-4" />
 *   <AlertTitle>Error</AlertTitle>
 *   <AlertDescription>
 *     Your session has expired. Please log in again.
 *   </AlertDescription>
 * </Alert>
 *
 * // Success alert
 * <Alert variant="success">
 *   <CheckCircle className="h-4 w-4" />
 *   <AlertTitle>Success</AlertTitle>
 *   <AlertDescription>
 *     Your changes have been saved.
 *   </AlertDescription>
 * </Alert>
 *
 * // Informational alert
 * <Alert variant="info">
 *   <Info className="h-4 w-4" />
 *   <AlertTitle>Note</AlertTitle>
 *   <AlertDescription>
 *     This feature is in beta.
 *   </AlertDescription>
 * </Alert>
 * ```
 */
import * as React from 'react';
import classy from '@/lib/primitives/classy';
import { alert, type AlertConfig, type AlertVariant } from '@/components/ui/alert.behavior';
import {
  alertActionClasses,
  alertClasses,
  alertDescriptionClasses,
  alertTitleClasses,
} from '@/components/ui/alert.classes';

export interface AlertProps extends React.HTMLAttributes<HTMLDivElement> {
  variant?: AlertVariant;
}

export const Alert = React.forwardRef<HTMLDivElement, AlertProps>(
  ({ variant, className, ...props }, ref) => {
    const config: AlertConfig = { variant };
    const classes = alertClasses(config, {});

    // A static score: role="alert" is constant, no state, no ids, no effects.
    // Config in, classes + aria out -- the card/container shape, no
    // useBehavior. The aria projection ignores ids, so pass an empty one.
    const { root: aria } = alert.aria({}, config, { root: '' });

    return (
      <div
        ref={ref}
        data-part="root"
        className={classy(classes.root, className)}
        {...aria}
        {...props}
      />
    );
  },
);

Alert.displayName = 'Alert';

export type AlertTitleProps = React.HTMLAttributes<HTMLHeadingElement>;

/** Renders a raw h5 because Typography's H5 does not exist yet in the new
 *  tree (matrix: typography, pending); the tag matches the shadcn surface. */
export const AlertTitle = React.forwardRef<HTMLHeadingElement, AlertTitleProps>(
  ({ className, ...props }, ref) => (
    <h5
      ref={ref}
      data-slot="alert-title"
      className={classy(alertTitleClasses, className)}
      {...props}
    />
  ),
);

AlertTitle.displayName = 'AlertTitle';

export type AlertDescriptionProps = React.HTMLAttributes<HTMLDivElement>;

export const AlertDescription = React.forwardRef<HTMLDivElement, AlertDescriptionProps>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      data-slot="alert-description"
      className={classy(alertDescriptionClasses, className)}
      {...props}
    />
  ),
);

AlertDescription.displayName = 'AlertDescription';

export type AlertActionProps = React.HTMLAttributes<HTMLDivElement>;

/** Trailing slot for a single dismiss/undo control -- oracle contract.
 *  Plain composition, like Title and Description: the behavior declares
 *  exactly one part (root), so this carries no data-part marker -- a
 *  binding rendering an undeclared part is structure the score never
 *  authorized (boundary 5). The `data-slot` marker is not a part -- it is the
 *  name the Astro and Web Component performances give the same region, so the
 *  three surfaces are assertable against one another. */
export const AlertAction = React.forwardRef<HTMLDivElement, AlertActionProps>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      data-slot="alert-action"
      className={classy(alertActionClasses, className)}
      {...props}
    />
  ),
);

AlertAction.displayName = 'AlertAction';

export default Alert;
