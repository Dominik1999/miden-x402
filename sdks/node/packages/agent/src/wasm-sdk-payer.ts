/**
 * Reference `Payer` implementation that wraps `@miden-sdk/miden-sdk` (WASM).
 *
 * Imported via dynamic `import()` so this package does not force a hard
 * dependency on the WASM SDK at install time — users that bring their own
 * payer (e.g. a Rust subprocess for the smoke test) don't pay the WASM
 * weight.
 *
 * Tested by the M4 risk-gate run described in `PLAN.md` §11: the
 * `@miden-sdk/miden-sdk` call MUST produce a note shape the facilitator's
 * `P2idNoteStorage::try_from` accepts. If that check fails, the integration
 * point to revisit is here, not the facilitator.
 *
 * The WASM SDK API surface is small but its types are not in DefinitelyTyped;
 * we treat the imported module as `unknown` and shape it at the boundary
 * with explicit assertions, so future SDK breakage produces a runtime
 * `WasmSdkPayerError` rather than a silent miscompile.
 */

import type { HexId } from '@miden-x402/types';

import type {
  Payer,
  P2idPaymentReceipt,
  P2idPaymentRequest,
  P2idUnprovenReceipt,
  P2idUnprovenRequest,
} from './payer.js';

export interface WasmSdkPayerOptions {
  /** Buyer (sender) account id this payer pays from. */
  buyerAccountId: HexId;
  /** Filesystem path or browser URL for the Miden client's local store. */
  storePath: string;
  /**
   * Polling interval (ms) when waiting for the create-note tx to commit.
   * Defaults to 2000.
   */
  pollIntervalMs?: number;
  /**
   * Timeout (ms) for waiting on a single create-note tx to commit.
   * Defaults to 120_000.
   */
  commitTimeoutMs?: number;
}

export class WasmSdkPayerError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = 'WasmSdkPayerError';
  }
}

/**
 * Build a `Payer` that uses `@miden-sdk/miden-sdk` to build, prove, and
 * submit a public P2ID note. The first call dynamically imports the SDK,
 * so consumers that supply their own `Payer` never load the WASM module.
 *
 * The exact SDK method names below are what `@miden-sdk/miden-sdk`
 * documents as of the M4 risk-gate validation. If a future SDK release
 * renames a method, the surface to update is this single file.
 */
export function createWasmSdkPayer(opts: WasmSdkPayerOptions): Payer {
  const pollMs = opts.pollIntervalMs ?? 2_000;
  const timeoutMs = opts.commitTimeoutMs ?? 120_000;
  let clientPromise: Promise<MidenClientLike> | null = null;

  async function getClient(): Promise<MidenClientLike> {
    if (!clientPromise) {
      clientPromise = loadMidenSdk(opts.storePath);
    }
    return clientPromise;
  }

  return {
    async payP2ID(request: P2idPaymentRequest): Promise<P2idPaymentReceipt> {
      const client = await getClient();
      const noteType = request.noteType ?? 'public';

      let submitted: SubmittedTx;
      try {
        submitted = await client.sendP2idNote({
          sender: opts.buyerAccountId,
          target: request.payTo,
          faucet: request.asset,
          amount: BigInt(request.amount),
          noteType,
        });
      } catch (e) {
        throw new WasmSdkPayerError(
          `failed to build/submit P2ID note via @miden-sdk/miden-sdk: ${(e as Error).message}`,
          { cause: e },
        );
      }

      const noteId: HexId = submitted.noteId;
      const transactionId: HexId = submitted.transactionId;
      const sender: HexId = submitted.sender ?? opts.buyerAccountId;

      const blockNum = await waitForCommit({
        client,
        noteId,
        timeoutMs,
        pollMs,
      });

      // For private notes, export the canonical NoteFile blob from the SDK
      // so the agent can put it into `PrivateP2idPayload.noteBlob`. The exact
      // SDK call signature is the M4 risk-gate validated one; if the SDK
      // renames the export method, update this single line.
      let noteBlob: string | undefined;
      if (noteType === 'private') {
        if (typeof client.exportNoteFile !== 'function') {
          throw new WasmSdkPayerError(
            "@miden-sdk/miden-sdk did not export `exportNoteFile`; the installed version does not support private-note export. Pin to a version that does.",
          );
        }
        try {
          noteBlob = await client.exportNoteFile(noteId, 'NoteDetails');
        } catch (e) {
          throw new WasmSdkPayerError(
            `failed to export private NoteFile via @miden-sdk/miden-sdk: ${(e as Error).message}`,
            { cause: e },
          );
        }
      }

      return { noteId, transactionId, sender, blockNum, noteBlob };
    },

    async payP2IDUnproven(request: P2idUnprovenRequest): Promise<P2idUnprovenReceipt> {
      const client = await getClient();
      if (typeof client.buildSignedUnprovenP2id !== 'function') {
        throw new WasmSdkPayerError(
          "@miden-sdk/miden-sdk did not export `buildSignedUnprovenP2id`; the installed version does not support the Guardian (verify-before-prove) flow. Pin to a version that does.",
        );
      }
      try {
        const built = await client.buildSignedUnprovenP2id({
          sender: opts.buyerAccountId,
          target: request.payTo,
          faucet: request.asset,
          amount: BigInt(request.amount),
          serialNum: request.serialNum,
        });
        return {
          txInputs: built.txInputs,
          signature: built.signature,
          signedSummary: built.signedSummary,
          expectedNoteBlob: built.expectedNoteBlob,
          transactionId: built.transactionId,
          sender: built.sender ?? opts.buyerAccountId,
        };
      } catch (e) {
        throw new WasmSdkPayerError(
          `failed to build signed unproven P2ID tx via @miden-sdk/miden-sdk: ${(e as Error).message}`,
          { cause: e },
        );
      }
    },
  };
}

interface SubmittedTx {
  noteId: HexId;
  transactionId: HexId;
  sender?: HexId;
}

interface CommittedNote {
  blockNum: number;
}

interface MidenClientLike {
  sendP2idNote(args: {
    sender: HexId;
    target: HexId;
    faucet: HexId;
    amount: bigint;
    noteType: 'public' | 'private';
  }): Promise<SubmittedTx>;

  getNote(noteId: HexId): Promise<CommittedNote | null>;

  /**
   * Exports a committed note as a base64-encoded canonical NoteFile blob.
   * Required for private-note payments; optional for the public path.
   * Mirror of `OutputNoteRecord::into_note_file(NoteExportType::NoteDetails)`
   * in the Rust facilitator's `pay_and_verify` binary.
   */
  exportNoteFile?(noteId: HexId, exportType: 'NoteId' | 'NoteDetails' | 'NoteWithProof'): Promise<string>;

  /**
   * Phase B / Guardian: builds a private P2ID note using the supplied
   * server-issued `serialNum`, runs the tx locally to compute the
   * `TransactionSummary`, signs that summary, but does NOT prove or submit.
   * Returns the canonical blobs the Guardian needs to verify offline and
   * later prove + submit.
   *
   * The WASM SDK version installed by `@miden-x402/agent` consumers must
   * expose this method for the Guardian flow to work; older versions
   * throw `WasmSdkPayerError`.
   */
  buildSignedUnprovenP2id?(args: {
    sender: HexId;
    target: HexId;
    faucet: HexId;
    amount: bigint;
    serialNum: HexId;
  }): Promise<{
    txInputs: string;
    signature: string;
    signedSummary: string;
    expectedNoteBlob: string;
    transactionId: HexId;
    sender?: HexId;
  }>;
}

async function loadMidenSdk(storePath: string): Promise<MidenClientLike> {
  let mod: unknown;
  try {
    // Deferred to runtime so consumers that bring their own `Payer` don't
    // pay the WASM weight. The specifier is held in a variable so TS does
    // not try to resolve it during typecheck — `@miden-sdk/miden-sdk` is a
    // peer dependency installed only by consumers using this default payer.
    const moduleSpecifier = '@miden-sdk/miden-sdk';
    mod = await import(moduleSpecifier);
  } catch (e) {
    throw new WasmSdkPayerError(
      'failed to import @miden-sdk/miden-sdk — install it as a peer dependency to use the default WASM payer',
      { cause: e },
    );
  }

  const factory = (mod as { createClient?: (opts: unknown) => Promise<unknown> }).createClient;
  if (typeof factory !== 'function') {
    throw new WasmSdkPayerError(
      "@miden-sdk/miden-sdk did not export the expected `createClient` factory; pin the SDK to the version validated in the M4 risk gate",
    );
  }

  const client = (await factory({ network: 'testnet', storePath })) as MidenClientLike;
  return client;
}

async function waitForCommit(args: {
  client: MidenClientLike;
  noteId: HexId;
  pollMs: number;
  timeoutMs: number;
}): Promise<number> {
  const deadline = Date.now() + args.timeoutMs;
  while (Date.now() < deadline) {
    const note = await args.client.getNote(args.noteId);
    if (note && note.blockNum > 0) {
      return note.blockNum;
    }
    await new Promise((r) => setTimeout(r, args.pollMs));
  }
  throw new WasmSdkPayerError(
    `note ${args.noteId} did not commit within ${args.timeoutMs}ms`,
  );
}
