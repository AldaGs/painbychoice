//! Arithmetic in number fields: type `1920/2` or `-(10+5)*3` into any
//! [`num`] field and it takes the result.

/// A `DragValue` whose typed text is evaluated with [`calc`]. Every number
/// field in the editor is built through this, so all of them accept math.
pub(crate) fn num<N: egui::emath::Numeric>(value: &mut N) -> egui::DragValue<'_> {
    egui::DragValue::new(value).custom_parser(calc)
}

/// Evaluate `+ - * /`, parentheses and unary minus over decimal numbers, with
/// the usual precedence. `None` for anything else, a trailing operator, or a
/// result that isn't finite (so `1/0` is rejected, not stored as infinity).
pub(crate) fn calc(text: &str) -> Option<f64> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = Parser { chars: &chars, i: 0 };
    let v = p.sum()?;
    (p.peek().is_none() && v.is_finite()).then_some(v)
}

struct Parser<'a> {
    chars: &'a [char],
    i: usize,
}

impl Parser<'_> {
    /// The next non-space character. Spaces separate tokens but never join
    /// them, so `3 4` is an error rather than 34.
    fn peek(&mut self) -> Option<char> {
        while self.chars.get(self.i).is_some_and(|c| c.is_whitespace()) {
            self.i += 1;
        }
        self.chars.get(self.i).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.i += 1;
            let r = self.product()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.i += 1;
            let r = self.unary()?;
            v = if op == '*' { v * r } else { v / r };
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                Some(-self.unary()?)
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            '(' => {
                self.i += 1;
                let v = self.sum()?;
                (self.peek()? == ')').then(|| self.i += 1)?;
                Some(v)
            }
            _ => {
                let start = self.i;
                while self.chars.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                    self.i += 1;
                }
                self.chars[start..self.i].iter().collect::<String>().parse().ok()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::calc;

    #[test]
    fn evaluates_simple_math() {
        assert_eq!(calc("42"), Some(42.0));
        assert_eq!(calc(" 1920 / 2 "), Some(960.0));
        assert_eq!(calc("10/4+1"), Some(3.5));
        assert_eq!(calc("2+3*4"), Some(14.0), "precedence");
        assert_eq!(calc("(2+3)*4"), Some(20.0));
        assert_eq!(calc("-(2*3)"), Some(-6.0));
        assert_eq!(calc("5--2"), Some(7.0));
        assert_eq!(calc(".5*4"), Some(2.0));
        assert_eq!(calc("8-2-1"), Some(5.0), "left to right");
    }

    #[test]
    fn rejects_rubbish() {
        for bad in ["", "1/0", "2+", "(1", "1)", "abc", "1..2", "3 4"] {
            assert_eq!(calc(bad), None, "{bad:?}");
        }
    }
}
