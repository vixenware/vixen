// Session glue over the GENERATED vox client (src/generated/daemon.ts).
// One eval = one RPC with two channels: we hold the control Tx (Step/Resume)
// and iterate the events Rx. The daemon does the rest.

import { channel, type Tx, type Rx } from "@bearcove/vox-core";
import {
  connectDaemon,
  type DaemonClient,
  type DemandEvent,
  type EvalRequest,
  type StepCommand,
} from "./generated/daemon";

export interface EvalSession {
  control: Tx<StepCommand>;
  events: Rx<DemandEvent>;
  /// Resolves when the RPC itself completes (after Done/Failed).
  finished: Promise<void>;
}

export async function connect(url: string): Promise<DaemonClient> {
  return connectDaemon(url);
}

export function startEval(client: DaemonClient, req: EvalRequest): EvalSession {
  const [controlTx, controlRx] = channel<StepCommand>();
  const [eventsTx, eventsRx] = channel<DemandEvent>();
  const finished = client.eval(req, controlRx, eventsTx);
  return { control: controlTx, events: eventsRx, finished };
}
