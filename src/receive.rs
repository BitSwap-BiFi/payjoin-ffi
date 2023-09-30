use std::{
	collections::HashMap,
	sync::{Arc, Mutex, MutexGuard},
};

use payjoin::receive::{
	MaybeInputsOwned as PdkMaybeInputsOwned, MaybeInputsSeen as PdkMaybeInputsSeen,
	MaybeMixedInputScripts as PdkMaybeMixedInputScripts, OutputsUnknown as PdkOutputsUnknown,
	PayjoinProposal as PdkPayjoinProposal, UncheckedProposal as PdkUncheckedProposal,
};
use payjoin::Error as PdkError;

use crate::{
	transaction::{PartiallySignedTransaction, Transaction},
	Address, OutPoint, PayjoinError, ScriptBuf, TxOut,
};

pub trait CanBroadcast: Send + Sync {
	fn test_mempool_accept(&self, tx: Vec<u8>) -> Result<bool, PayjoinError>;
}

#[derive(Clone)]
pub struct Headers(HashMap<String, String>);

impl Headers {
	pub fn from_vec(body: Vec<u8>) -> Self {
		let mut h = HashMap::new();
		h.insert("content-type".to_string(), "text/plain".to_string());
		h.insert("content-length".to_string(), body.len().to_string());
		Headers(h)
	}
	pub fn get_map(&self) -> HashMap<String, String> {
		self.0.clone()
	}
}

impl payjoin::receive::Headers for Headers {
	fn get_header(&self, key: &str) -> Option<&str> {
		self.0.get(key).map(|e| e.as_str())
	}
}

/// The sender’s original PSBT and optional parameters
///
/// This type is used to proces the request. It is returned by UncheckedProposal::from_request().
///
/// If you are implementing an interactive payment processor, you should get extract the original transaction with get_transaction_to_schedule_broadcast() and schedule, followed by checking that the transaction can be broadcast with check_can_broadcast. Otherwise it is safe to call assume_interactive_receive to proceed with validation.
pub struct UncheckedProposal {
	internal: Mutex<Option<PdkUncheckedProposal>>,
}

impl From<PdkUncheckedProposal> for UncheckedProposal {
	fn from(value: PdkUncheckedProposal) -> Self {
		Self { internal: Mutex::new(Some(value)) }
	}
}

impl UncheckedProposal {
	pub fn get_configuration(
		&self,
	) -> (Option<PdkUncheckedProposal>, MutexGuard<Option<PdkUncheckedProposal>>) {
		let mut data_guard = self.internal.lock().unwrap();
		(std::mem::replace(&mut *data_guard, None), data_guard)
	}
	pub fn from_request(
		body: Vec<u8>, query: String, headers: Arc<Headers>,
	) -> Result<Self, PayjoinError> {
		match PdkUncheckedProposal::from_request(
			body.as_slice(),
			query.as_str(),
			(*headers).clone(),
		) {
			Ok(e) => Ok(e.into()),
			Err(e) => Err(PayjoinError::RequestError { message: e.to_string() }),
		}
	}

	/// The Sender’s Original PSBT
	pub fn get_transaction_to_schedule_broadcast(&self) -> Arc<Transaction> {
		Arc::new(
			self.internal
				.lock()
				.unwrap()
				.as_ref()
				.unwrap()
				.get_transaction_to_schedule_broadcast()
				.into(),
		)
	}

	/// Call after checking that the Original PSBT can be broadcast.
	///
	/// Receiver MUST check that the Original PSBT from the sender can be broadcast, i.e. testmempoolaccept bitcoind rpc returns { “allowed”: true,.. } for get_transaction_to_check_broadcast() before calling this method.
	///
	/// Do this check if you generate bitcoin uri to receive Payjoin on sender request without manual human approval, like a payment processor. Such so called “non-interactive” receivers are otherwise vulnerable to probing attacks. If a sender can make requests at will, they can learn which bitcoin the receiver owns at no cost. Broadcasting the Original PSBT after some time in the failure case makes incurs sender cost and prevents probing.
	///
	/// Call this after checking downstream.
	pub fn check_can_broadcast(
		&self, can_broadcast: Box<dyn CanBroadcast>,
	) -> Result<Arc<MaybeInputsOwned>, PayjoinError> {
		let (proposal, _) = Self::get_configuration(self);
		let res = proposal.unwrap().check_can_broadcast(|tx| {
			match can_broadcast
				.test_mempool_accept(payjoin::bitcoin::consensus::encode::serialize(&tx))
			{
				Ok(e) => Ok(e),
				Err(e) => Err(PdkError::Server(e.into())),
			}
		});
		match res {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::UnexpectedError { message: e.to_string() }),
		}
	}

	/// Call this method if the only way to initiate a Payjoin with this receiver requires manual intervention, as in most consumer wallets.
	///
	/// So-called “non-interactive” receivers, like payment processors, that allow arbitrary requests are otherwise vulnerable to probing attacks. Those receivers call get_transaction_to_check_broadcast() and attest_tested_and_scheduled_broadcast() after making those checks downstream.
	pub fn assume_interactive_receiver(&self) -> Arc<MaybeInputsOwned> {
		let (proposal, _) = Self::get_configuration(self);
		match proposal {
			Some(e) => Arc::new(e.assume_interactive_receiver().into()),
			None => panic!("Unexpected Error: checkCanBroadcast already called"),
		}
	}
}

/// Typestate to validate that the Original PSBT has no receiver-owned inputs.

/// Call check_no_receiver_owned_inputs() to proceed.
pub struct MaybeInputsOwned {
	internal: Mutex<Option<PdkMaybeInputsOwned>>,
}

impl From<PdkMaybeInputsOwned> for MaybeInputsOwned {
	fn from(value: PdkMaybeInputsOwned) -> Self {
		MaybeInputsOwned { internal: Mutex::new(Some(value)) }
	}
}

pub trait IsScriptOwned: Send + Sync {
	fn is_owned(&self, script: Arc<ScriptBuf>) -> Result<bool, PayjoinError>;
}

impl MaybeInputsOwned {
	fn get_owned_inputs(
		&self,
	) -> (Option<PdkMaybeInputsOwned>, MutexGuard<Option<PdkMaybeInputsOwned>>) {
		let mut data_guard = self.internal.lock().unwrap();
		(std::mem::replace(&mut *data_guard, None), data_guard)
	}
	/// Check that the Original PSBT has no receiver-owned inputs. Return original-psbt-rejected error or otherwise refuse to sign undesirable inputs.
	///
	/// An attacker could try to spend receiver’s own inputs. This check prevents that.
	pub fn check_inputs_not_owned(
		&self, is_owned: Box<dyn IsScriptOwned>,
	) -> Result<Arc<MaybeMixedInputScripts>, PayjoinError> {
		let (owned_inputs, _) = Self::get_owned_inputs(self);
		match owned_inputs.unwrap().check_inputs_not_owned(|input| {
			let res = is_owned.is_owned(Arc::new(ScriptBuf { internal: input.to_owned() }));
			match res {
				Ok(e) => Ok(e),
				Err(e) => Err(PdkError::Server(e.into())),
			}
		}) {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::ServerError { message: e.to_string() }),
		}
	}
}

/// Typestate to validate that the Original PSBT has no inputs that have been seen before.
///
/// Call check_no_inputs_seen to proceed.
pub struct MaybeMixedInputScripts {
	internal: Mutex<Option<PdkMaybeMixedInputScripts>>,
}

impl From<PdkMaybeMixedInputScripts> for MaybeMixedInputScripts {
	fn from(value: PdkMaybeMixedInputScripts) -> Self {
		MaybeMixedInputScripts { internal: Mutex::new(Some(value)) }
	}
}

impl MaybeMixedInputScripts {
	fn get_input_scripts(
		&self,
	) -> (Option<PdkMaybeMixedInputScripts>, MutexGuard<Option<PdkMaybeMixedInputScripts>>) {
		let mut data_guard = self.internal.lock().unwrap();
		(std::mem::replace(&mut *data_guard, None), data_guard)
	}
	/// Verify the original transaction did not have mixed input types Call this after checking downstream.
	///
	/// Note: mixed spends do not necessarily indicate distinct wallet fingerprints. This check is intended to prevent some types of wallet fingerprinting.
	pub fn check_no_mixed_input_scripts(&self) -> Result<Arc<MaybeInputsSeen>, PayjoinError> {
		let (input_scripts, _) = Self::get_input_scripts(self);
		match input_scripts.unwrap().check_no_mixed_input_scripts() {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::ReceiveError { message: e.to_string() }),
		}
	}
}

pub trait IsOutputKnown {
	fn is_known(&self, outpoint: OutPoint) -> Result<bool, PayjoinError>;
}

/// Typestate to validate that the Original PSBT has no inputs that have been seen before.
///
/// Call check_no_inputs_seen to proceed.
pub struct MaybeInputsSeen {
	internal: Mutex<Option<PdkMaybeInputsSeen>>,
}

impl From<PdkMaybeInputsSeen> for MaybeInputsSeen {
	fn from(value: PdkMaybeInputsSeen) -> Self {
		MaybeInputsSeen { internal: Mutex::new(Some(value)) }
	}
}

impl MaybeInputsSeen {
	fn get_inputs(&self) -> (Option<PdkMaybeInputsSeen>, MutexGuard<Option<PdkMaybeInputsSeen>>) {
		let mut data_guard = self.internal.lock().unwrap();
		(std::mem::replace(&mut *data_guard, None), data_guard)
	}
	/// Make sure that the original transaction inputs have never been seen before. This prevents probing attacks. This prevents reentrant Payjoin, where a sender proposes a Payjoin PSBT as a new Original PSBT for a new Payjoin.
	pub fn check_no_inputs_seen_before(
		&self, is_known: Box<dyn IsOutputKnown>,
	) -> Result<Arc<OutputsUnknown>, PayjoinError> {
		let (inputs, _) = Self::get_inputs(self);
		match inputs.unwrap().check_no_inputs_seen_before(|outpoint| {
			let res = is_known.is_known(outpoint.to_owned().into());
			match res {
				Ok(e) => Ok(e),
				Err(e) => Err(PdkError::Server(e.into())),
			}
		}) {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::ReceiveError { message: e.to_string() }),
		}
	}
}

/// The receiver has not yet identified which outputs belong to the receiver.
///
/// Only accept PSBTs that send us money. Identify those outputs with identify_receiver_outputs() to proceed

pub struct OutputsUnknown {
	internal: Mutex<Option<PdkOutputsUnknown>>,
}

impl From<PdkOutputsUnknown> for OutputsUnknown {
	fn from(value: PdkOutputsUnknown) -> Self {
		OutputsUnknown { internal: Mutex::new(Some(value)) }
	}
}

impl OutputsUnknown {
	fn get_unknown_outputs(
		&self,
	) -> (Option<PdkOutputsUnknown>, MutexGuard<Option<PdkOutputsUnknown>>) {
		let mut data_guard = self.internal.lock().unwrap();
		(std::mem::replace(&mut *data_guard, None), data_guard)
	}
	/// Find which outputs belong to the receiver
	pub fn identify_receiver_outputs(
		&self, is_receiver_output: Box<dyn IsScriptOwned>,
	) -> Result<Arc<PayjoinProposal>, PayjoinError> {
		let (unknown_outputs, _) = Self::get_unknown_outputs(self);
		match unknown_outputs.unwrap().identify_receiver_outputs(|output_script| {
			let res = is_receiver_output
				.is_owned(Arc::new(ScriptBuf { internal: output_script.to_owned() }));
			match res {
				Ok(e) => Ok(e),
				Err(e) => Err(PdkError::Server(e.into())),
			}
		}) {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::ReceiveError { message: e.to_string() }),
		}
	}
}

/// A mutable checked proposal that the receiver may contribute inputs to to make a payjoin.
pub struct PayjoinProposal {
	internal: Mutex<Option<PdkPayjoinProposal>>,
}

impl From<PdkPayjoinProposal> for PayjoinProposal {
	fn from(value: PdkPayjoinProposal) -> Self {
		PayjoinProposal { internal: Mutex::new(Some(value)) }
	}
}

impl PayjoinProposal {
	fn get_proposal(&self) -> Option<PdkPayjoinProposal> {
		let mut data_guard = self.internal.lock().unwrap();
		std::mem::replace(&mut *data_guard, None)
	}
	fn get_proposal_mutex_guard(&self) -> MutexGuard<Option<PdkPayjoinProposal>> {
		self.internal.lock().unwrap()
	}

	pub fn is_output_substitution_disabled(&self) -> bool {
		let mut guard = self.get_proposal_mutex_guard();
		guard.as_mut().unwrap().is_output_substitution_disabled()
	}

	pub fn contribute_witness_input(&self, txout: TxOut, outpoint: OutPoint) {
		let mut guard = self.get_proposal_mutex_guard();
		guard.as_mut().unwrap().contribute_witness_input(txout.into(), outpoint.into());
	}

	pub fn contribute_non_witness_input(&self, tx: Arc<Transaction>, outpoint: OutPoint) {
		let mut guard = self.get_proposal_mutex_guard();
		guard.as_mut().unwrap().contribute_non_witness_input((*tx).clone().into(), outpoint.into())
	}

	pub fn substitute_output_address(&self, substitute_address: Arc<Address>) {
		let mut guard = self.get_proposal_mutex_guard();
		guard.as_mut().unwrap().substitute_output_address((*substitute_address).clone().into())
	}

	/// Apply additional fee contribution now that the receiver has contributed input this is kind of a “build_proposal” step before we sign and finalize and extract
	///
	/// WARNING: DO NOT ALTER INPUTS OR OUTPUTS AFTER THIS STEP
	pub fn apply_fee(
		&self, min_feerate_sat_per_vb: Option<u64>,
	) -> Result<Arc<PartiallySignedTransaction>, PayjoinError> {
		let mut guard = self.get_proposal_mutex_guard();
		match guard.as_mut().unwrap().apply_fee(min_feerate_sat_per_vb) {
			Ok(e) => Ok(Arc::new(e.to_owned().into())),
			Err(e) => Err(PayjoinError::RequestError { message: e.to_string() }),
		}
	}

	/// Return a Payjoin Proposal PSBT that the sender will find acceptable.
	///
	/// This attempts to calculate any network fee owed by the receiver, subtract it from their output, and return a PSBT that can produce a consensus-valid transaction that the sender will accept.
	///
	/// wallet_process_psbt should sign and finalize receiver inputs
	pub fn prepare_psbt(
		&self, processed_psbt: Arc<PartiallySignedTransaction>,
	) -> Result<Arc<PartiallySignedTransaction>, PayjoinError> {
		let proposal = self.get_proposal();
		if proposal.is_none() {
			panic!("PayjoinProposal not initalized");
		}
		match proposal.unwrap().prepare_psbt((*processed_psbt).clone().into()) {
			Ok(e) => Ok(Arc::new(e.into())),
			Err(e) => Err(PayjoinError::RequestError { message: e.to_string() }),
		}
	}

	/// Select receiver input such that the payjoin avoids surveillance. Return the input chosen that has been applied to the Proposal.
	///
	/// Proper coin selection allows payjoin to resemble ordinary transactions. To ensure the resemblence, a number of heuristics must be avoided.
	///
	/// UIH “Unecessary input heuristic” is one class of them to avoid. We define UIH1 and UIH2 according to the BlockSci practice BlockSci UIH1 and UIH2:
	pub fn try_preserving_privacy(
		&self, candidate_inputs: HashMap<u64, OutPoint>,
	) -> Result<OutPoint, PayjoinError> {
		let mut _candidate_inputs: HashMap<payjoin::bitcoin::Amount, payjoin::bitcoin::OutPoint> =
			HashMap::new();
		for (key, value) in candidate_inputs.iter() {
			_candidate_inputs.insert(
				payjoin::bitcoin::Amount::from_sat(key.to_owned()),
				value.to_owned().into(),
			);
		}
		let mut guard = self.get_proposal_mutex_guard();
		match guard.as_mut().unwrap().try_preserving_privacy(_candidate_inputs) {
			Ok(e) => Ok(OutPoint { txid: e.txid.to_string(), vout: e.vout }),
			Err(_) => Err(PayjoinError::SelectionError),
		}
	}
	// TODO - pub fn utxos_to_be_locked(&self)
}

#[cfg(test)]
mod test {
	use std::sync::Arc;

	use crate::Network;

	use super::*;

	fn get_proposal_from_test_vector() -> Result<UncheckedProposal, PayjoinError> {
		// OriginalPSBT Test Vector from BIP
		// | InputScriptType | Orginal PSBT Fee rate | maxadditionalfeecontribution | additionalfeeoutputindex|
		// |-----------------|-----------------------|------------------------------|-------------------------|
		// | P2SH-P2WPKH     |  2 sat/vbyte          | 0.00000182                   | 0                       |
		let original_psbt =
            "cHNidP8BAHMCAAAAAY8nutGgJdyYGXWiBEb45Hoe9lWGbkxh/6bNiOJdCDuDAAAAAAD+////AtyVuAUAAAAAF6kUHehJ8GnSdBUOOv6ujXLrWmsJRDCHgIQeAAAAAAAXqRR3QJbbz0hnQ8IvQ0fptGn+votneofTAAAAAAEBIKgb1wUAAAAAF6kU3k4ekGHKWRNbA1rV5tR5kEVDVNCHAQcXFgAUx4pFclNVgo1WWAdN1SYNX8tphTABCGsCRzBEAiB8Q+A6dep+Rz92vhy26lT0AjZn4PRLi8Bf9qoB/CMk0wIgP/Rj2PWZ3gEjUkTlhDRNAQ0gXwTO7t9n+V14pZ6oljUBIQMVmsAaoNWHVMS02LfTSe0e388LNitPa1UQZyOihY+FFgABABYAFEb2Giu6c4KO5YW0pfw3lGp9jMUUAAA=";

		let body = original_psbt.as_bytes();
		let headers = Headers::from_vec(body.to_vec());
		UncheckedProposal::from_request(
			body.to_vec(),
			"?maxadditionalfeecontribution=182?additionalfeeoutputindex=0".to_string(),
			Arc::new(headers),
		)
	}

	#[test]
	fn can_get_proposal_from_request() {
		let proposal = get_proposal_from_test_vector();
		assert!(proposal.is_ok(), "OriginalPSBT should be a valid request");
	}

	struct MockScriptOwned {}

	struct MockOutputOwned {}

	impl IsOutputKnown for MockOutputOwned {
		fn is_known(&self, _: OutPoint) -> Result<bool, PayjoinError> {
			Ok(false)
		}
	}

	impl IsScriptOwned for MockScriptOwned {
		fn is_owned(&self, script: Arc<ScriptBuf>) -> Result<bool, PayjoinError> {
			{
				let network = Network::Bitcoin;
				Ok(Address::from_script(script, network).unwrap()
					== Address::new("3CZZi7aWFugaCdUCS15dgrUUViupmB8bVM".to_owned()).unwrap())
			}
		}
	}

	#[test]
	fn unchecked_proposal_unlocks_after_checks() {
		let proposal = get_proposal_from_test_vector().unwrap();
		let payjoin = proposal
			.assume_interactive_receiver()
			.clone()
			.check_inputs_not_owned(Box::new(MockScriptOwned {}))
			.expect("No inputs should be owned")
			.check_no_mixed_input_scripts()
			.expect("No mixed input scripts")
			.check_no_inputs_seen_before(Box::new(MockOutputOwned {}))
			.expect("No inputs should be seen before")
			.identify_receiver_outputs(Box::new(MockScriptOwned {}))
			.expect("Receiver output should be identified");
		let payjoin = payjoin.apply_fee(None);

		assert!(payjoin.is_ok(), "Payjoin should be a valid PSBT");
	}
}
