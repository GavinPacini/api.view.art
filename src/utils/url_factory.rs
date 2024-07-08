use {
    adjectives::ADJECTIVES,
    colors::COLORS,
    ethers::types::Address,
    nouns::NOUNS,
    std::fmt::Write,
};

mod adjectives;
mod colors;
mod nouns;

const ADDRESS_TAKE: usize = 7;
const BITS_PER_WORD: usize = 10;

pub fn generate_url_from_address(address: Address) -> String {
    let address_str = format!("{:?}", address);
    let shortened_address = &address_str[2..];
    let address_segment = &shortened_address[..ADDRESS_TAKE];
    let sanitized = address_segment.replace('-', "");
    let binary: String = sanitized.chars().fold(String::new(), |mut output, h| {
        let _ = write!(output, "{:04b}", h.to_digit(16).unwrap_or(0));
        output
    });

    let regex_pattern = format!(".{{1,{}}}", BITS_PER_WORD);
    let binary_chunks: Vec<String> = regex::Regex::new(&regex_pattern)
        .unwrap()
        .find_iter(&binary)
        .map(|mat| mat.as_str().to_string())
        .collect();

    let mapped_words: Vec<&str> = binary_chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let index_value = usize::from_str_radix(chunk, 2).unwrap_or(0);

            match index {
                0 => ADJECTIVES[index_value % ADJECTIVES.len()],
                1 => COLORS[index_value % COLORS.len()],
                2 => NOUNS[index_value % NOUNS.len()],
                _ => "", // This shouldn't happen with the first 7 characters of an Ethereum address
            }
        })
        .collect();

    mapped_words
        .into_iter()
        .filter(|word| !word.is_empty())
        .collect::<Vec<&str>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_name() {
        let address = "0x140EccA86Cb294eE86F836E4e9C0b5b80Aa8c21F"
            .parse::<Address>()
            .unwrap();
        let name = generate_url_from_address(address);
        assert!(name == "bossy-hyacinth-ethics");
    }
}
