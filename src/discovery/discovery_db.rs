use std::{
  time::Duration,
  collections::{hash_map::Iter as HashIter, HashMap},
  iter::Map,
  slice::Iter,
};

use crate::{
  dds::qos::HasQoSPolicy, structure::guid::EntityId,
  network::util::get_local_unicast_socket_address, network::util::get_local_multicast_locators,
};

use crate::structure::{guid::GUID, duration::Duration as SDuration, entity::Entity};

use crate::{
  network::constant::{get_user_traffic_unicast_port, get_user_traffic_multicast_port},
  dds::{
    rtps_reader_proxy::RtpsReaderProxy, reader::Reader, participant::DomainParticipant,
    topic::Topic, rtps_writer_proxy::RtpsWriterProxy,
  },
};

use super::data_types::{
  topic_data::{
    SubscriptionBuiltinTopicData, DiscoveredReaderData, ReaderProxy, DiscoveredWriterData,
    DiscoveredTopicData, TopicBuiltinTopicData,
  },
  spdp_participant_data::SPDPDiscoveredParticipantData,
};

pub struct DiscoveryDB {
  participant_proxies: HashMap<GUID, SPDPDiscoveredParticipantData>,
  // local writer proxies for topics (topic name acts as key)
  local_topic_writers: Vec<DiscoveredWriterData>,
  // local reader proxies for topics (topic name acts as key)
  local_topic_readers: Vec<DiscoveredReaderData>,

  writers_reader_proxies: HashMap<GUID, Vec<DiscoveredReaderData>>,
  readers_writer_proxies: HashMap<GUID, Vec<DiscoveredWriterData>>,

  topics: HashMap<String, DiscoveredTopicData>,

  readers_updated: bool,
  writers_updated: bool,
}

impl DiscoveryDB {
  pub fn new() -> DiscoveryDB {
    DiscoveryDB {
      participant_proxies: HashMap::new(),
      local_topic_writers: Vec::new(),
      local_topic_readers: Vec::new(),
      writers_reader_proxies: HashMap::new(),
      readers_writer_proxies: HashMap::new(),
      topics: HashMap::new(),
      readers_updated: false,
      writers_updated: false,
    }
  }

  pub fn update_participant(&mut self, data: &SPDPDiscoveredParticipantData) -> bool {
    let data = data.clone();

    match data.participant_guid {
      Some(guid) => {
        self.participant_proxies.insert(guid, data);
        true
      }
      _ => false,
    }
  }

  pub fn participant_cleanup(&mut self) {
    let instant = time::precise_time_ns();
    self.participant_proxies.retain(|_, d| {
      let ld = match &d.lease_duration {
        Some(ld) => ld.clone(),
        // default is 100 secs
        None => SDuration::from(Duration::from_secs(100)),
      };

      SDuration::from(Duration::from_nanos(instant - d.updated_time)) < ld
    });
  }

  pub fn topic_cleanup(&mut self) {
    let instant = time::precise_time_ns();
    self.topics = self
      .topics
      .iter()
      .filter(|(name, p)| {
        let dur = match &p.topic_data.lifespan {
          Some(ls) => ls.duration.clone(),
          // TODO: what is default lifespan
          None => SDuration::from(Duration::from_secs(100)),
        };
        SDuration::from(Duration::from_nanos(instant - p.updated_time)) < dur
          || self
            .local_topic_readers
            .iter()
            .filter(|drd| {
              let tname = match drd.subscription_topic_data.topic_name.as_ref() {
                Some(tname) => tname,
                None => return false,
              };
              *tname == **name
            })
            .count()
            > 0
          || self
            .local_topic_writers
            .iter()
            .filter(|dwd| {
              let tname = match dwd.publication_topic_data.topic_name.as_ref() {
                Some(tname) => tname,
                None => return false,
              };
              *tname == **name
            })
            .count()
            > 0
      })
      .map(|(n, p)| (n.clone(), p.clone()))
      .collect();
  }

  pub fn get_participants<'a>(
    &'a self,
  ) -> Map<
    HashIter<'a, GUID, SPDPDiscoveredParticipantData>,
    fn((&GUID, &'a SPDPDiscoveredParticipantData)) -> &'a SPDPDiscoveredParticipantData,
  > {
    type cvfun<'a> =
      fn((&GUID, &'a SPDPDiscoveredParticipantData)) -> &'a SPDPDiscoveredParticipantData;
    fn conver<'a>(
      (_, data): (&GUID, &'a SPDPDiscoveredParticipantData),
    ) -> &'a SPDPDiscoveredParticipantData {
      data
    }
    let cv: cvfun = conver;
    let a = self.participant_proxies.iter().map(cv);
    a
  }

  pub fn update_local_topic_writer(&mut self, writer: DiscoveredWriterData) {
    let dbwriter = self
      .local_topic_writers
      .iter_mut()
      .find(|p| p.writer_proxy.remote_writer_guid == writer.writer_proxy.remote_writer_guid);

    match dbwriter {
      Some(v) => {
        *v = writer;
        return;
      }
      None => self.local_topic_writers.push(writer),
    };

    self.writers_updated = true;
  }

  pub fn remove_local_topic_writer(&mut self, guid: GUID) {
    let pos =
      self
        .local_topic_writers
        .iter()
        .position(|w| match w.writer_proxy.remote_writer_guid {
          Some(g) => g == guid,
          None => false,
        });

    match pos {
      Some(pos) => {
        self.local_topic_writers.remove(pos);
      }
      None => (),
    };

    self.writers_updated = true;
  }

  pub fn get_writers_reader_proxies<'a>(
    &'a self,
    guid: GUID,
  ) -> Option<Iter<'a, DiscoveredReaderData>> {
    self.writers_reader_proxies.get(&guid).map(|p| p.iter())
  }

  pub fn get_readers_writer_proxies<'a>(
    &'a self,
    guid: GUID,
  ) -> Option<Iter<'a, DiscoveredWriterData>> {
    self.readers_writer_proxies.get(&guid).map(|p| p.iter())
  }

  pub fn update_subscription(&mut self, data: &DiscoveredReaderData) -> bool {
    let topic_name = match data.subscription_topic_data.topic_name.as_ref() {
      Some(v) => v,
      None => {
        println!("Failed to update subscription. No topic name.");
        println!("{:?}", data.subscription_topic_data);
        return false;
      }
    };

    let writers: Vec<DiscoveredWriterData> = self
      .local_topic_writers
      .iter()
      .filter(|p| {
        let ptopic_name = p.publication_topic_data.topic_name.as_ref();
        match ptopic_name {
          Some(tn) => *tn == *topic_name,
          None => false,
        }
      })
      .map(|p| p.clone())
      .collect();

    let rtps_reader_proxy = match RtpsReaderProxy::from_discovered_reader_data(&data) {
      Some(v) => v,
      None => {
        println!("Failed to update subscription. Cannot parse RtpsReaderProxy.");
        return false;
      }
    };

    for writer in writers.into_iter() {
      let guid = match writer.writer_proxy.remote_writer_guid {
        Some(guid) => guid,
        None => {
          println!("warning: Writer doesn't have GUID.");
          continue;
        }
      };

      // update writers reader proxy
      self.update_writers_reader_proxy(guid, data.clone());

      // update writers local data
      let wrp = self.writers_reader_proxies.get_mut(&guid);

      match wrp {
        Some(v) => v
          .iter_mut()
          .filter(|p| {
            let guid = match &p.reader_proxy.remote_reader_guid {
              Some(guid) => guid,
              None => return false,
            };
            *guid == rtps_reader_proxy.remote_reader_guid
          })
          .for_each(|p| p.update(&rtps_reader_proxy)),
        None => {
          self
            .writers_reader_proxies
            .insert(guid.clone(), vec![data.clone()]);
          continue;
        }
      };
    }

    true
  }

  pub fn update_publication(&mut self, data: &DiscoveredWriterData) -> bool {
    let topic_name = match data.publication_topic_data.topic_name.as_ref() {
      Some(v) => v,
      None => {
        println!("Failed to update publication. No topic name.");
        return false;
      }
    };

    let readers: Vec<DiscoveredReaderData> = self
      .local_topic_readers
      .iter()
      .filter(|p| {
        let ptopic_name = p.subscription_topic_data.topic_name.as_ref();
        match ptopic_name {
          Some(tn) => *tn == *topic_name,
          None => false,
        }
      })
      .map(|p| p.clone())
      .collect();

    let rtps_writer_proxy = match RtpsWriterProxy::from(&data) {
      Some(v) => v,
      None => {
        println!("Failed to update publication. Cannot parse RtpsWriterProxy.");
        return false;
      }
    };

    for reader in readers.into_iter() {
      let guid = match &reader.reader_proxy.remote_reader_guid {
        Some(guid) => guid,
        None => {
          println!("warning: Reader doesn't have GUID");
          continue;
        }
      };

      // update reader writer proxy
      self.update_readers_writer_proxy(guid, data.clone());

      let rwp = self.readers_writer_proxies.get_mut(guid);

      match rwp {
        Some(v) => v
          .iter_mut()
          .filter(|p| match p.writer_proxy.remote_writer_guid {
            Some(g) => g == rtps_writer_proxy.remote_writer_guid,
            None => false,
          })
          .for_each(|p| p.update(&rtps_writer_proxy)),
        None => {
          self
            .readers_writer_proxies
            .insert(guid.clone(), vec![data.clone()]);
          continue;
        }
      };
    }

    true
  }

  pub fn update_topic_data_drd(&mut self, drd: &DiscoveredReaderData) {
    let topic_data = DiscoveredTopicData::new(TopicBuiltinTopicData {
      key: None,
      name: drd.subscription_topic_data.topic_name.clone(),
      type_name: drd.subscription_topic_data.type_name.clone(),
      durability: drd.subscription_topic_data.durability.clone(),
      deadline: drd.subscription_topic_data.deadline.clone(),
      latency_budget: drd.subscription_topic_data.latency_budget.clone(),
      liveliness: drd.subscription_topic_data.liveliness.clone(),
      reliability: drd.subscription_topic_data.reliability.clone(),
      lifespan: drd.subscription_topic_data.lifespan.clone(),
      destination_order: drd.subscription_topic_data.destination_order.clone(),
      presentation: drd.subscription_topic_data.presentation.clone(),
      history: None,
      resource_limits: None,
      ownership: drd.subscription_topic_data.ownership.clone(),
    });

    self.update_topic_data(&topic_data);
  }

  pub fn update_topic_data_dwd(&mut self, dwd: &DiscoveredWriterData) {
    let topic_data = DiscoveredTopicData::new(TopicBuiltinTopicData {
      key: None,
      name: dwd.publication_topic_data.topic_name.clone(),
      type_name: dwd.publication_topic_data.type_name.clone(),
      durability: dwd.publication_topic_data.durability.clone(),
      deadline: dwd.publication_topic_data.deadline.clone(),
      latency_budget: dwd.publication_topic_data.latency_budget.clone(),
      liveliness: dwd.publication_topic_data.liveliness.clone(),
      reliability: dwd.publication_topic_data.reliability.clone(),
      lifespan: dwd.publication_topic_data.lifespan.clone(),
      destination_order: dwd.publication_topic_data.destination_order.clone(),
      presentation: dwd.publication_topic_data.presentation.clone(),
      history: None,
      resource_limits: None,
      ownership: dwd.publication_topic_data.ownership.clone(),
    });

    self.update_topic_data(&topic_data);
  }

  pub fn update_topic_data_p(&mut self, topic: &Topic) {
    let topic_data = DiscoveredTopicData::new(TopicBuiltinTopicData {
      key: None,
      name: Some(String::from(topic.get_name())),
      type_name: Some(String::from(topic.get_type().name())),
      durability: topic.get_qos().durability.clone(),
      deadline: topic.get_qos().deadline.clone(),
      latency_budget: topic.get_qos().latency_budget.clone(),
      liveliness: topic.get_qos().liveliness.clone(),
      reliability: topic.get_qos().reliability.clone(),
      lifespan: topic.get_qos().lifespan.clone(),
      destination_order: topic.get_qos().destination_order.clone(),
      presentation: topic.get_qos().presentation.clone(),
      history: topic.get_qos().history.clone(),
      resource_limits: topic.get_qos().resource_limits.clone(),
      ownership: topic.get_qos().ownership.clone(),
    });

    self.update_topic_data(&topic_data);
  }

  pub fn update_topic_data(&mut self, data: &DiscoveredTopicData) -> bool {
    let topic_name = match &data.topic_data.name {
      Some(n) => n,
      None => {
        println!("Received DiscoveredTopicData doesn't have a name.");
        return false;
      }
    };

    match self.topics.get_mut(topic_name) {
      Some(t) => *t = data.clone(),
      None => {
        self.topics.insert(topic_name.clone(), data.clone());
      }
    };

    true
  }

  pub fn initialize_participant_reader_proxy(&mut self, writer_guid: GUID, port: u16) {
    let guid = writer_guid.from_prefix(EntityId::ENTITYID_SPDP_BUILTIN_PARTICIPANT_READER);
    let mut reader_proxy = ReaderProxy::new(guid);
    reader_proxy.multicast_locator_list = get_local_multicast_locators(port);

    let sub_topic_data = SubscriptionBuiltinTopicData::new(
      guid,
      GUID::GUID_UNKNOWN,
      &String::from("DCPSParticipant"),
      &String::from("SPDPDiscoveredParticipantData"),
    );
    let drd = DiscoveredReaderData {
      reader_proxy,
      subscription_topic_data: sub_topic_data,
      content_filter: None,
    };
    self.writers_reader_proxies.insert(writer_guid, vec![drd]);
  }

  pub fn update_writers_reader_proxy(
    &mut self,
    writer: GUID,
    reader: DiscoveredReaderData,
  ) -> bool {
    let mut proxies = match self.writers_reader_proxies.get(&writer) {
      Some(prox) => prox.clone(),
      None => Vec::new(),
    };

    let guid = match &reader.reader_proxy.remote_reader_guid {
      Some(g) => g,
      None => {
        println!("Failed to update writers reader proxy. No reader guid.");
        return false;
      }
    };

    let value: Option<usize> =
      proxies
        .iter()
        .position(|x| match x.reader_proxy.remote_reader_guid {
          Some(xguid) => xguid == *guid,
          None => false,
        });
    match value {
      Some(val) => proxies[val] = reader,
      None => proxies.push(reader),
    };

    self.writers_reader_proxies.insert(writer.clone(), proxies);
    true
  }

  pub fn update_readers_writer_proxy(
    &mut self,
    reader: &GUID,
    writer: DiscoveredWriterData,
  ) -> bool {
    let mut proxies = match self.readers_writer_proxies.get(&reader) {
      Some(prox) => prox.clone(),
      None => Vec::new(),
    };

    let guid = match &writer.writer_proxy.remote_writer_guid {
      Some(g) => g,
      None => {
        println!("Failed to update readers writer proxy. No writer guid.");
        return false;
      }
    };

    let value: Option<usize> =
      proxies
        .iter()
        .position(|x| match x.writer_proxy.remote_writer_guid {
          Some(xguid) => xguid == *guid,
          None => false,
        });

    match value {
      Some(val) => proxies[val] = writer,
      None => proxies.push(writer),
    };

    self.readers_writer_proxies.insert(reader.clone(), proxies);
    true
  }

  // local topic readers
  pub fn update_local_topic_reader(
    &mut self,
    domain_participant: &DomainParticipant,
    topic: &Topic,
    reader: &Reader,
  ) {
    let reader_guid = reader.get_guid();

    let mut reader_proxy = RtpsReaderProxy::new(reader_guid);

    let unicast_locators = get_local_unicast_socket_address(get_user_traffic_unicast_port(
      domain_participant.domain_id(),
      domain_participant.participant_id(),
    ));

    let multicast_locators = get_local_multicast_locators(get_user_traffic_multicast_port(
      domain_participant.domain_id(),
    ));

    reader_proxy.unicast_locator_list = unicast_locators;
    reader_proxy.multicast_locator_list = multicast_locators;
    // TODO: remove these constant evaluations
    reader_proxy.expects_in_line_qos = false;
    reader_proxy.is_active = true;

    let subscription_data = SubscriptionBuiltinTopicData {
      key: Some(reader_guid),
      participant_key: Some(domain_participant.get_guid()),
      topic_name: Some(String::from(topic.get_name())),
      type_name: Some(String::from(topic.get_type().name())),
      durability: topic.get_qos().durability,
      deadline: topic.get_qos().deadline,
      latency_budget: topic.get_qos().latency_budget,
      liveliness: topic.get_qos().liveliness,
      reliability: topic.get_qos().reliability,
      ownership: topic.get_qos().ownership,
      destination_order: topic.get_qos().destination_order,
      time_based_filter: topic.get_qos().time_based_filter.clone(),
      presentation: topic.get_qos().presentation,
      lifespan: topic.get_qos().lifespan,
    };

    // TODO: possibly change content filter to dynamic value
    let content_filter = None;

    let discovered_reader_data = DiscoveredReaderData {
      reader_proxy: ReaderProxy::from(reader_proxy),
      subscription_topic_data: subscription_data,
      content_filter,
    };

    let topic_name = String::from(topic.get_name());

    // updating local topic readers
    let mut treaders: Vec<&mut DiscoveredReaderData> = self
      .local_topic_readers
      .iter_mut()
      .filter(|p| {
        *match p.subscription_topic_data.topic_name.as_ref() {
          Some(s) => s,
          None => return false,
        } == topic_name
      })
      .filter(|p| {
        *match p.reader_proxy.remote_reader_guid.as_ref() {
          Some(g) => g,
          None => return false,
        } == reader_guid
      })
      .collect();

    if !treaders.is_empty() {
      treaders
        .iter_mut()
        .filter(|p| match p.reader_proxy.remote_reader_guid { Some(g) => g, None => return false } == reader_guid)
        .for_each(|p| **p = discovered_reader_data.clone());
    } else {
      self.local_topic_readers.push(discovered_reader_data);
    }

    self.readers_updated = true;
  }

  pub fn remove_local_topic_reader(&mut self, guid: GUID) {
    let pos =
      self
        .local_topic_readers
        .iter()
        .position(|p| match p.reader_proxy.remote_reader_guid {
          Some(g) => g == guid,
          None => false,
        });

    match pos {
      Some(pos) => {
        self.local_topic_readers.remove(pos);
      }
      None => (),
    };

    self.readers_updated = true;
  }

  pub fn is_readers_updated(&self) -> bool {
    self.readers_updated
  }

  pub fn readers_updated(&mut self, updated: bool) {
    self.readers_updated = updated;
  }

  pub fn is_writers_updated(&self) -> bool {
    self.writers_updated
  }

  pub fn writers_updated(&mut self, updated: bool) {
    self.writers_updated = updated;
  }

  pub fn get_all_local_topic_readers<'a>(&'a self) -> Iter<'a, DiscoveredReaderData> {
    self.local_topic_readers.iter()
  }

  pub fn get_all_local_topic_writers<'a>(&'a self) -> Iter<'a, DiscoveredWriterData> {
    self.local_topic_writers.iter()
  }

  pub fn get_all_topics<'a>(
    &'a self,
  ) -> Map<
    HashIter<'a, String, DiscoveredTopicData>,
    fn((&'a String, &'a DiscoveredTopicData)) -> &'a DiscoveredTopicData,
  > {
    self.topics.iter().map(|(_, v)| v)
  }

  // TODO: return iterator somehow?
  pub fn get_local_topic_readers<'a>(&'a self, topic: &'a Topic) -> Vec<&DiscoveredReaderData> {
    let topic_name = String::from(topic.get_name());
    self
      .local_topic_readers
      .iter()
      .filter(|p| {
        *match p.subscription_topic_data.topic_name.as_ref() {
          Some(t) => t,
          None => return false,
        } == topic_name
      })
      .collect()
  }
}

#[cfg(test)]

mod tests {
  use super::*;

  use crate::{
    structure::dds_cache::DDSCache,
    test::{
      random_data::RandomData,
      test_data::{subscription_builtin_topic_data, spdp_participant_data, reader_proxy_data},
    },
    dds::{qos::QosPolicies, typedesc::TypeDesc},
  };
  use std::sync::{RwLock, Arc};

  use crate::structure::guid::*;
  use crate::serialization::cdrSerializer::CDR_serializer_adapter;
  use byteorder::LittleEndian;

  #[test]
  fn discdb_participant_operations() {
    let mut discoverydb = DiscoveryDB::new();
    let mut data = spdp_participant_data().unwrap();
    data.lease_duration = Some(SDuration::from(Duration::from_secs(1)));

    discoverydb.update_participant(&data);
    assert!(discoverydb.participant_proxies.len() == 1);

    discoverydb.update_participant(&data);
    assert!(discoverydb.participant_proxies.len() == 1);

    std::thread::sleep(Duration::from_secs(2));
    discoverydb.participant_cleanup();
    assert!(discoverydb.participant_proxies.len() == 0);

    // TODO: more operations tests
  }

  #[test]
  fn discdb_writer_proxies() {
    let mut discoverydb = DiscoveryDB::new();
    let topic_name = String::from("some_topic");
    let type_name = String::from("RandomData");
    let dreader = DiscoveredReaderData::default(&topic_name, &type_name);
    discoverydb.update_writers_reader_proxy(GUID::new(), dreader);
    assert_eq!(discoverydb.writers_reader_proxies.len(), 1);

    // TODO: more tests :)
  }

  #[test]
  fn discdb_subscription_operations() {
    let mut discovery_db = DiscoveryDB::new();

    let domain_participant = DomainParticipant::new(7, 0);
    let topic = domain_participant
      .create_topic(
        "Foobar",
        TypeDesc::new(String::from("RandomData")),
        &QosPolicies::qos_none(),
      )
      .unwrap();
    let topic2 = domain_participant
      .create_topic(
        "Barfoo",
        TypeDesc::new(String::from("RandomData")),
        &QosPolicies::qos_none(),
      )
      .unwrap();

    let publisher1 = domain_participant
      .create_publisher(&QosPolicies::qos_none())
      .unwrap();
    let dw = publisher1
      .create_datawriter::<RandomData, CDR_serializer_adapter<RandomData, LittleEndian>>(
        None,
        &topic,
        &QosPolicies::qos_none(),
      )
      .unwrap();

    let writer_data = DiscoveredWriterData::new(&dw, &topic, &domain_participant);

    let writer_key = writer_data.writer_proxy.remote_writer_guid.unwrap().clone();
    discovery_db.update_local_topic_writer(writer_data);
    assert_eq!(discovery_db.local_topic_writers.len(), 1);

    let publisher2 = domain_participant
      .create_publisher(&QosPolicies::qos_none())
      .unwrap();
    let dw2 = publisher2
      .create_datawriter::<RandomData, CDR_serializer_adapter<RandomData, LittleEndian>>(
        None,
        &topic,
        &QosPolicies::qos_none(),
      )
      .unwrap();
    let writer_data2 = DiscoveredWriterData::new(&dw2, &topic, &domain_participant);
    let _writer2_key = writer_data2
      .writer_proxy
      .remote_writer_guid
      .unwrap()
      .clone();
    discovery_db.update_local_topic_writer(writer_data2);
    assert_eq!(discovery_db.local_topic_writers.len(), 2);

    // creating data
    let reader1 = reader_proxy_data().unwrap();
    let mut reader1sub = subscription_builtin_topic_data().unwrap();
    reader1sub.key = reader1.remote_reader_guid.clone();
    reader1sub.topic_name = Some(topic.get_name().to_string().clone());
    let dreader1 = DiscoveredReaderData {
      reader_proxy: reader1.clone(),
      subscription_topic_data: reader1sub.clone(),
      content_filter: None,
    };
    discovery_db.update_subscription(&dreader1);
    assert_eq!(
      discovery_db
        .writers_reader_proxies
        .get(&writer_key)
        .unwrap()
        .len(),
      1
    );

    let reader2 = reader_proxy_data().unwrap();
    let mut reader2sub = subscription_builtin_topic_data().unwrap();
    reader2sub.key = reader2.remote_reader_guid.clone();
    reader2sub.topic_name = Some(topic2.get_name().to_string().clone());
    let dreader2 = DiscoveredReaderData {
      reader_proxy: reader2,
      subscription_topic_data: reader2sub,
      content_filter: None,
    };
    discovery_db.update_subscription(&dreader2);
    assert_eq!(
      discovery_db
        .writers_reader_proxies
        .get(&writer_key)
        .unwrap()
        .len(),
      1
    );
    let writers_size: usize = discovery_db
      .writers_reader_proxies
      .values()
      .map(|p| p.len())
      .sum();
    assert_eq!(writers_size, 2);

    let reader3 = reader1.clone();
    let reader3sub = reader1sub.clone();
    let dreader3 = DiscoveredReaderData {
      reader_proxy: reader3,
      subscription_topic_data: reader3sub,
      content_filter: None,
    };
    discovery_db.update_subscription(&dreader3);
    assert_eq!(
      discovery_db
        .writers_reader_proxies
        .get(&writer_key)
        .unwrap()
        .len(),
      1
    );

    // TODO: there might be a need for different scenarios
  }

  #[test]
  fn discdb_local_topic_reader() {
    let dp = DomainParticipant::new(12, 0);
    let topic = dp
      .create_topic(
        "some topic name",
        TypeDesc::new(String::from("Wazzup")),
        &QosPolicies::qos_none(),
      )
      .unwrap();
    let mut discoverydb = DiscoveryDB::new();

    let (notification_sender, _notification_receiver) = mio_extras::channel::sync_channel(100);
    let reader = Reader::new(
      GUID::new(),
      notification_sender.clone(),
      Arc::new(RwLock::new(DDSCache::new())),
      topic.get_name().to_string(),
    );

    discoverydb.update_local_topic_reader(&dp, &topic, &reader);
    assert_eq!(discoverydb.local_topic_readers.len(), 1);
    assert_eq!(discoverydb.get_local_topic_readers(&topic).len(), 1);

    discoverydb.update_local_topic_reader(&dp, &topic, &reader);
    assert_eq!(discoverydb.local_topic_readers.len(), 1);
    assert_eq!(discoverydb.get_local_topic_readers(&topic).len(), 1);

    let reader = Reader::new(
      GUID::new(),
      notification_sender.clone(),
      Arc::new(RwLock::new(DDSCache::new())),
      topic.get_name().to_string(),
    );

    discoverydb.update_local_topic_reader(&dp, &topic, &reader);
    assert_eq!(discoverydb.get_local_topic_readers(&topic).len(), 2);
    assert_eq!(discoverydb.get_all_local_topic_readers().len(), 2);
  }
}
