use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiplierGroup {
    UsDc,
    Canada,
    OtherNorthAmerica,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiplierDefinition {
    pub id: &'static str,
    pub code: &'static str,
    pub display_name: &'static str,
    pub group: MultiplierGroup,
}

macro_rules! multiplier {
    ($code:literal, $name:literal, $group:ident) => {
        MultiplierDefinition {
            id: $code,
            code: $code,
            display_name: $name,
            group: MultiplierGroup::$group,
        }
    };
    ($id:literal, $code:literal, $name:literal, $group:ident) => {
        MultiplierDefinition {
            id: $id,
            code: $code,
            display_name: $name,
            group: MultiplierGroup::$group,
        }
    };
}

pub const MULTIPLIERS: &[MultiplierDefinition] = &[
    multiplier!("AL", "Alabama", UsDc),
    multiplier!("AK", "Alaska", UsDc),
    multiplier!("AZ", "Arizona", UsDc),
    multiplier!("AR", "Arkansas", UsDc),
    multiplier!("CA", "California", UsDc),
    multiplier!("CO", "Colorado", UsDc),
    multiplier!("CT", "Connecticut", UsDc),
    multiplier!("DE", "Delaware", UsDc),
    multiplier!("FL", "Florida", UsDc),
    multiplier!("GA", "Georgia", UsDc),
    multiplier!("US-HI", "HI", "Hawaii", UsDc),
    multiplier!("ID", "Idaho", UsDc),
    multiplier!("IL", "Illinois", UsDc),
    multiplier!("IN", "Indiana", UsDc),
    multiplier!("IA", "Iowa", UsDc),
    multiplier!("KS", "Kansas", UsDc),
    multiplier!("KY", "Kentucky", UsDc),
    multiplier!("LA", "Louisiana", UsDc),
    multiplier!("ME", "Maine", UsDc),
    multiplier!("MD", "Maryland", UsDc),
    multiplier!("MA", "Massachusetts", UsDc),
    multiplier!("MI", "Michigan", UsDc),
    multiplier!("MN", "Minnesota", UsDc),
    multiplier!("MS", "Mississippi", UsDc),
    multiplier!("MO", "Missouri", UsDc),
    multiplier!("MT", "Montana", UsDc),
    multiplier!("NE", "Nebraska", UsDc),
    multiplier!("NV", "Nevada", UsDc),
    multiplier!("NH", "New Hampshire", UsDc),
    multiplier!("NJ", "New Jersey", UsDc),
    multiplier!("NM", "New Mexico", UsDc),
    multiplier!("NY", "New York", UsDc),
    multiplier!("NC", "North Carolina", UsDc),
    multiplier!("ND", "North Dakota", UsDc),
    multiplier!("OH", "Ohio", UsDc),
    multiplier!("OK", "Oklahoma", UsDc),
    multiplier!("OR", "Oregon", UsDc),
    multiplier!("PA", "Pennsylvania", UsDc),
    multiplier!("RI", "Rhode Island", UsDc),
    multiplier!("SC", "South Carolina", UsDc),
    multiplier!("SD", "South Dakota", UsDc),
    multiplier!("TN", "Tennessee", UsDc),
    multiplier!("TX", "Texas", UsDc),
    multiplier!("UT", "Utah", UsDc),
    multiplier!("VT", "Vermont", UsDc),
    multiplier!("VA", "Virginia", UsDc),
    multiplier!("WA", "Washington", UsDc),
    multiplier!("WV", "West Virginia", UsDc),
    multiplier!("WI", "Wisconsin", UsDc),
    multiplier!("WY", "Wyoming", UsDc),
    multiplier!("DC", "District of Columbia", UsDc),
    multiplier!("AB", "Alberta", Canada),
    multiplier!("BC", "British Columbia", Canada),
    multiplier!("MB", "Manitoba", Canada),
    multiplier!("NB", "New Brunswick", Canada),
    multiplier!("NL", "Newfoundland and Labrador", Canada),
    multiplier!("NS", "Nova Scotia", Canada),
    multiplier!("NT", "Northwest Territories", Canada),
    multiplier!("NU", "Nunavut", Canada),
    multiplier!("ON", "Ontario", Canada),
    multiplier!("PE", "Prince Edward Island", Canada),
    multiplier!("QC", "Quebec", Canada),
    multiplier!("SK", "Saskatchewan", Canada),
    multiplier!("YT", "Yukon", Canada),
    multiplier!("4U1U", "United Nations Headquarters", OtherNorthAmerica),
    multiplier!("6Y", "Jamaica", OtherNorthAmerica),
    multiplier!("8P", "Barbados", OtherNorthAmerica),
    multiplier!("C6", "Bahamas", OtherNorthAmerica),
    multiplier!("CM", "Cuba", OtherNorthAmerica),
    multiplier!("CY9", "Saint Paul Island", OtherNorthAmerica),
    multiplier!("CY0", "Sable Island", OtherNorthAmerica),
    multiplier!("FG", "Guadeloupe", OtherNorthAmerica),
    multiplier!("FJ", "Saint Barthelemy", OtherNorthAmerica),
    multiplier!("FM", "Martinique", OtherNorthAmerica),
    multiplier!("FO", "Clipperton Island", OtherNorthAmerica),
    multiplier!("FP", "Saint Pierre and Miquelon", OtherNorthAmerica),
    multiplier!("FS", "Saint Martin", OtherNorthAmerica),
    multiplier!("HH", "Haiti", OtherNorthAmerica),
    multiplier!("DXCC-HI", "HI", "Dominican Republic", OtherNorthAmerica),
    multiplier!("HK0", "San Andres and Providencia", OtherNorthAmerica),
    multiplier!("HP", "Panama", OtherNorthAmerica),
    multiplier!("HR", "Honduras", OtherNorthAmerica),
    multiplier!("J3", "Grenada", OtherNorthAmerica),
    multiplier!("J6", "Saint Lucia", OtherNorthAmerica),
    multiplier!("J7", "Dominica", OtherNorthAmerica),
    multiplier!("J8", "Saint Vincent", OtherNorthAmerica),
    multiplier!("KG4", "Guantanamo Bay", OtherNorthAmerica),
    multiplier!("KP1", "Navassa Island", OtherNorthAmerica),
    multiplier!("KP2", "US Virgin Islands", OtherNorthAmerica),
    multiplier!("KP4", "Puerto Rico", OtherNorthAmerica),
    multiplier!("KP5", "Desecheo Island", OtherNorthAmerica),
    multiplier!("OX", "Greenland", OtherNorthAmerica),
    multiplier!("PJ5", "Saba and Saint Eustatius", OtherNorthAmerica),
    multiplier!("PJ7", "Sint Maarten", OtherNorthAmerica),
    multiplier!("TG", "Guatemala", OtherNorthAmerica),
    multiplier!("TI", "Costa Rica", OtherNorthAmerica),
    multiplier!("TI9", "Cocos Island", OtherNorthAmerica),
    multiplier!("V2", "Antigua and Barbuda", OtherNorthAmerica),
    multiplier!("V3", "Belize", OtherNorthAmerica),
    multiplier!("V4", "Saint Kitts and Nevis", OtherNorthAmerica),
    multiplier!("VP2E", "Anguilla", OtherNorthAmerica),
    multiplier!("VP2M", "Montserrat", OtherNorthAmerica),
    multiplier!("VP2V", "British Virgin Islands", OtherNorthAmerica),
    multiplier!("VP5", "Turks and Caicos Islands", OtherNorthAmerica),
    multiplier!("VP9", "Bermuda", OtherNorthAmerica),
    multiplier!("XE", "Mexico", OtherNorthAmerica),
    multiplier!("XF4", "Revillagigedo Islands", OtherNorthAmerica),
    multiplier!("YN", "Nicaragua", OtherNorthAmerica),
    multiplier!("YS", "El Salvador", OtherNorthAmerica),
    multiplier!("YV0", "Aves Island", OtherNorthAmerica),
    multiplier!("ZF", "Cayman Islands", OtherNorthAmerica),
];

pub fn find(code: &str) -> Option<&'static MultiplierDefinition> {
    MULTIPLIERS
        .iter()
        .find(|multiplier| multiplier.code == code)
}

pub fn resolve(
    code: &str,
    call: &str,
    country: Option<&str>,
) -> Option<&'static MultiplierDefinition> {
    let mut matches = MULTIPLIERS
        .iter()
        .filter(|multiplier| multiplier.code == code);
    let first = matches.next()?;
    let Some(second) = matches.next() else {
        return Some(first);
    };
    let dominican = call.trim().to_ascii_uppercase().starts_with("HI")
        || country.is_some_and(|country| country.trim().to_ascii_uppercase().contains("DOMINICAN"));
    [first, second]
        .into_iter()
        .find(|entry| {
            (dominican && entry.group == MultiplierGroup::OtherNorthAmerica)
                || (!dominican && entry.group == MultiplierGroup::UsDc)
        })
        .or(Some(first))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn catalog_has_111_unique_codes_and_all_groups() {
        assert_eq!(MULTIPLIERS.len(), 111);
        assert_eq!(
            MULTIPLIERS
                .iter()
                .map(|multiplier| multiplier.id)
                .collect::<BTreeSet<_>>()
                .len(),
            111
        );
        assert_eq!(
            MULTIPLIERS
                .iter()
                .map(|multiplier| multiplier.code)
                .collect::<BTreeSet<_>>()
                .len(),
            110
        );
        for group in [
            MultiplierGroup::UsDc,
            MultiplierGroup::Canada,
            MultiplierGroup::OtherNorthAmerica,
        ] {
            assert!(MULTIPLIERS.iter().any(|entry| entry.group == group));
        }
    }

    #[test]
    fn every_catalog_entry_has_display_metadata() {
        assert!(MULTIPLIERS.iter().all(|entry| {
            !entry.code.trim().is_empty() && !entry.display_name.trim().is_empty()
        }));
    }

    #[test]
    fn resolves_hawaii_and_dominican_republic_separately() {
        assert_eq!(resolve("HI", "KH6ABC", Some("Hawaii")).unwrap().id, "US-HI");
        assert_eq!(
            resolve("HI", "HI8ABC", Some("Dominican Republic"))
                .unwrap()
                .id,
            "DXCC-HI"
        );
    }
}
