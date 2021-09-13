
/// DeserializerAdapter is used to fit serde Deserializer implementations and DataReader together.
/// DataReader cannot assume a specific serialization format, so it needs to be given as a parameter.
///
/// for WITH_KEY topics, we need to be able to (de)serailize the key in addition to data.
pub mod no_key {
	use serde::de::DeserializeOwned;
	use serde::ser::Serialize;
	use bytes::Bytes;

	use crate::serialization::error::Result;
	use crate::messages::submessages::submessage_elements::serialized_payload::RepresentationIdentifier;

	pub trait DeserializerAdapter<D>
	where
	  D: DeserializeOwned,
	{
		/// Which data representations can the DeserializerAdapter read?
		/// See RTPS specification Section 10 and Table 10.3
	  fn supported_encodings() -> &'static [RepresentationIdentifier]; 

	  fn from_bytes<'de>(input_bytes: &'de [u8], encoding: RepresentationIdentifier) -> Result<D>;

	  /// This method has a default implementation, but the default will make a copy of
	  /// all the input data in memory and then call from_bytes() .
	  // In order to avoid the copy, implement also this method.
	  fn from_vec_bytes<'de>(input_vec_bytes: &'de [Bytes], encoding: RepresentationIdentifier) -> Result<D> {
	  	let total_len = input_vec_bytes.iter().map( |s| s.len()).sum();
	  	let mut total_payload = Vec::with_capacity(total_len);
	  	for iv in input_vec_bytes {
	  		total_payload.extend(iv)
	  	}
	  	Self::from_bytes(&total_payload, encoding)
	  }
	}

	pub trait SerializerAdapter<D>
	where
	  D: Serialize,
	{
		// what encoding do we produce?
	  fn output_encoding() -> RepresentationIdentifier;

	  fn to_Bytes(value: &D) -> Result<Bytes>;
	}

}

pub mod with_key {
	use serde::Serialize;
	use serde::de::DeserializeOwned;
	
	use bytes::Bytes;

	use crate::serialization::error::Result;
	use crate::dds::traits::key::*;
	use crate::messages::submessages::submessage_elements::serialized_payload::RepresentationIdentifier;

	use super::no_key;

	pub trait DeserializerAdapter<D> : no_key::DeserializerAdapter<D>
	where
	  D: Keyed + DeserializeOwned,
	{
	  fn key_from_bytes<'de>(input_bytes: &'de [u8], encoding: RepresentationIdentifier) -> Result<D::K>;
	}

	pub trait SerializerAdapter<D> : no_key::SerializerAdapter<D>
	where
	  D: Keyed + Serialize,
	{
	  fn key_to_Bytes(value: &D::K) -> Result<Bytes>;
	}

}
