use hickory_proto::{
    op::{Message, MessageType, ResponseCode},
    rr::{
        Name, RData, Record, RecordType,
        rdata::{A, AAAA},
    },
};
use std::{net::IpAddr, str::FromStr};

pub fn build_dns_response(mut request: Message, domain: &str, ip: IpAddr, ttl: u32) -> Result<Message, String> {
    let name = Name::from_str(domain).map_err(|e| e.to_string())?;
    let record = match ip {
        IpAddr::V4(ip) => Record::from_rdata(name.clone(), ttl, RData::A(A(ip))),
        IpAddr::V6(ip) => Record::from_rdata(name, ttl, RData::AAAA(AAAA(ip))),
    };

    // We must indicate that this message is a response. Otherwise, implementations may not
    // recognize it.
    request.metadata.message_type = MessageType::Response;

    // Only answer the question that was asked. A fake address handed back for a CNAME/PTR/MX/...
    // query is read by the client as that record type (Wine's dnsapi takes the 4 address bytes as
    // a name pointer and crashes - Roblox Studio under Wine died at startup). Anything other than
    // an address query gets an empty NOERROR answer instead.
    let wanted = request.queries.first().map(|q| q.query_type());
    let fits = match (wanted, &record.data) {
        (Some(RecordType::A), RData::A(_)) | (Some(RecordType::AAAA), RData::AAAA(_)) | (Some(RecordType::ANY), _) => true,
        _ => false,
    };
    if fits {
        request.add_answer(record);
    }
    Ok(request)
}

pub fn remove_ipv6_entries(message: &mut Message) {
    message.answers.retain(|answer| !matches!(&answer.data, RData::AAAA(_)));
}

pub fn extract_ipaddr_from_dns_message(message: &Message) -> Result<IpAddr, String> {
    if message.metadata.response_code != ResponseCode::NoError {
        return Err(format!("{:?}", message.metadata.response_code));
    }
    let mut cname = None;
    for answer in &message.answers {
        match &answer.data {
            RData::A(addr) => {
                return Ok(IpAddr::V4((*addr).into()));
            }
            RData::AAAA(addr) => {
                return Ok(IpAddr::V6((*addr).into()));
            }
            RData::CNAME(name) => {
                cname = Some(name.to_utf8());
            }
            _ => {}
        }
    }
    if let Some(cname) = cname {
        return Err(cname);
    }
    Err(format!("{:?}", message.answers))
}

pub fn extract_domain_from_dns_message(message: &Message) -> Result<String, String> {
    let query = message.queries.first().ok_or("DnsRequest no query body")?;
    let name = query.name().to_string();
    Ok(name)
}

pub fn parse_data_to_dns_message(data: &[u8], used_by_tcp: bool) -> Result<Message, String> {
    if used_by_tcp {
        if data.len() < 2 {
            return Err("invalid dns data".into());
        }
        let len = u16::from_be_bytes([data[0], data[1]]) as usize;
        let data = data.get(2..len + 2).ok_or("invalid dns data")?;
        return parse_data_to_dns_message(data, false);
    }
    let message = Message::from_vec(data).map_err(|e| e.to_string())?;
    Ok(message)
}
