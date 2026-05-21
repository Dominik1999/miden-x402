use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::FeltRng;
use miden_protocol::account::AccountId;
use miden_protocol::note::{
    Note, NoteAssets, NoteMetadata, NoteRecipient, NoteStorage, NoteTag, NoteType,
};
use miden_protocol::errors::NoteError;

use crate::types::AgentDebitNoteStorage;

/// The MASM source for the AgentDebitNote script.
pub const AGENT_DEBIT_NOTE_MASM: &str = include_str!("../masm/agent_debit_note.masm");

/// Create an AgentDebitNote — a pre-funded note that can be consumed by anyone
/// who presents a valid agent signature over (note_serial_num, merchant, amount).
///
/// The note script enforces:
///   - Before expiry: creates P2ID to merchant + remainder AgentDebitNote
///   - After expiry: allows user to reclaim full balance
pub fn create<R: FeltRng>(
    sender: AccountId,
    note_script: miden_protocol::note::NoteScript,
    storage: AgentDebitNoteStorage,
    assets: Vec<Asset>,
    note_type: NoteType,
    rng: &mut R,
) -> Result<Note, NoteError> {
    let serial_num = rng.draw_word();
    let note_storage: NoteStorage = storage.into();
    let recipient = NoteRecipient::new(serial_num, note_script, note_storage);
    let tag = NoteTag::new(0);
    let metadata = NoteMetadata::new(sender, note_type).with_tag(tag);
    let vault = NoteAssets::new(assets)?;
    Ok(Note::new(vault, metadata, recipient))
}
